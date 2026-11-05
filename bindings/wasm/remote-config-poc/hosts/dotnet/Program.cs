// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

using System.Net.Http.Headers;
using System.Text.Json;
using System.Text.Json.Nodes;
using Wasmtime;
using RemoteConfigPoc;

using var engine = new Engine();
using var module = Module.FromFile(engine, args[0]);
using var linker = new Linker(engine);
linker.DefineWasi();
using var store = new Store(engine);
store.SetWasiConfiguration(new WasiConfiguration());
var instance = linker.Instantiate(store, module);
instance.GetAction("_initialize")?.Invoke();
var abi = new RcAbi(instance);
using var http = new HttpClient();
var scenario = JsonNode.Parse(File.ReadAllText(args[2])) ?? throw new InvalidOperationException("Missing scenario");
var options = scenario["options"]?.DeepClone() ?? throw new InvalidOperationException("Missing options");
options["url"] = args[1];
options["language"] = "dotnet";
int client = Input(abi, JsonSerializer.SerializeToElement(options), abi.rc_create);
try
{
    if (args.Length > 3 && args[3] == "--cancel-stdin")
    {
        using var cancellation = new CancellationTokenSource();
        Task<JsonElement> pending = Poll(abi, client, http, cancellation.Token);
        if (await Console.In.ReadLineAsync() != "cancel") throw new InvalidOperationException("Expected cancel command");
        cancellation.Cancel();
        Console.WriteLine((await pending).GetRawText());
        abi.Check(abi.rc_poll(client));
        abi.Check(abi.rc_cancel(client));
        ResolveOutcome(abi, abi.Json(abi.rc_result(client)));
        Console.WriteLine("{\"restarted\":true}");
    }
    else
    {
        foreach (var action in scenario["actions"]!.AsArray())
        {
            string kind = action!["kind"]!.GetValue<string>();
            JsonElement outcome;
            if (kind == "ack")
            {
                Input(abi, JsonSerializer.SerializeToElement(action["value"]), buffer => abi.rc_ack(client, buffer));
                outcome = JsonSerializer.SerializeToElement(new { acknowledged = true });
            }
            else if (kind is "cancel" or "timeout")
            {
                abi.Check(abi.rc_poll(client));
                JsonElement events = abi.Json(abi.rc_events(client));
                JsonElement operation = events.EnumerateArray().First(value => value.GetProperty("kind").GetString() == (kind == "timeout" ? "sleep" : "http"));
                int operationId = operation.GetProperty("id").GetInt32();
                if (kind == "cancel") abi.Check(abi.rc_cancel(client));
                else Input(abi, JsonSerializer.SerializeToElement(new { }), buffer => abi.rc_complete(client, operationId, buffer));
                outcome = abi.Json(abi.rc_result(client));
                int reply = abi.Write(JsonSerializer.SerializeToElement(new
                {
                    status = 200,
                    body = abi.WriteBytes(Convert.FromBase64String("e30="))
                }));
                try
                {
                    if (abi.rc_complete(client, operationId, reply) != -1) throw new InvalidOperationException("Late completion accepted");
                }
                finally { abi.Check(abi.rc_free(reply)); }
                if (!abi.Json(abi.rc_events(client)).EnumerateArray().Any(value => value.GetProperty("kind").GetString() == "cancel"))
                    throw new InvalidOperationException("No host cancellation");
            }
            else if (kind == "fixture")
            {
                abi.Check(abi.rc_poll(client));
                var requests = new List<JsonElement>();
                JsonElement? result = null;
                for (int step = 0; step < 32 && result is null; step++)
                {
                    JsonElement events = abi.Json(abi.rc_events(client));
                    bool hasHttp = events.EnumerateArray().Any(value => value.GetProperty("kind").GetString() == "http");
                    foreach (var operation in events.EnumerateArray())
                    {
                        if (operation.GetProperty("kind").GetString() == "sleep" && !hasHttp)
                        {
                            Input(abi, JsonSerializer.SerializeToElement(new { }), buffer =>
                                abi.rc_complete(client, operation.GetProperty("id").GetInt32(), buffer));
                        }
                        if (operation.GetProperty("kind").GetString() != "http") continue;
                        var recorded = JsonSerializer.SerializeToElement(new
                        {
                            kind = operation.GetProperty("kind").GetString(),
                            id = operation.GetProperty("id").GetInt32(),
                            method = operation.GetProperty("method").GetString(),
                            url = operation.GetProperty("url").GetString(),
                            headers = operation.GetProperty("headers").Clone(),
                            body = Convert.ToBase64String(abi.Read(operation.GetProperty("body").GetInt32()))
                        });
                        requests.Add(recorded);
                        string body = new Uri(operation.GetProperty("url").GetString()!).AbsolutePath == "/api/v0.1/org"
                            ? "" : action["body"]!.GetValue<string>();
                        Input(abi, JsonSerializer.SerializeToElement(new
                        {
                            status = 200,
                            body = abi.WriteBytes(Convert.FromBase64String(body))
                        }), buffer =>
                            abi.rc_complete(client, operation.GetProperty("id").GetInt32(), buffer));
                    }
                    int buffer = abi.Check(abi.rc_result(client));
                    if (buffer > 0) result = ResolveOutcome(abi, abi.Json(buffer));
                }
                if (result is null) throw new InvalidOperationException("Fixture did not complete");
                outcome = JsonSerializer.SerializeToElement(new { outcome = result, requests });
            }
            else if (kind == "poll") outcome = await Poll(abi, client, http);
            else throw new InvalidOperationException($"Unknown action: {kind}");
            Console.WriteLine(outcome.GetRawText());
        }
    }
}
finally { abi.Check(abi.rc_close(client)); }

static int Input(RcAbi abi, JsonElement value, Func<int, int> operation)
{
    int buffer = abi.Write(value);
    try { return abi.Check(operation(buffer)); }
    finally { abi.Check(abi.rc_free(buffer)); }
}

static JsonElement ResolveOutcome(RcAbi abi, JsonElement outcome)
{
    if (!outcome.TryGetProperty("changes", out var changes)) return outcome;
    return JsonSerializer.SerializeToElement(new
    {
        changes = changes.EnumerateArray().Select(change => new
        {
            kind = change.GetProperty("kind").GetString(),
            path = change.GetProperty("path").GetString(),
            version = change.GetProperty("version").GetString(),
            contentsBase64 = change.TryGetProperty("contents", out var contents)
                && contents.ValueKind == JsonValueKind.Number
                ? Convert.ToBase64String(abi.Read(contents.GetInt32()))
                : (string?)null
        })
    });
}

static async Task<JsonElement> Poll(RcAbi abi, int client, HttpClient http, CancellationToken cancellation = default)
{
    abi.Check(abi.rc_poll(client));
    var pending = new Dictionary<int, (CancellationTokenSource Cancellation, Task<(int Status, JsonElement Headers, byte[] Body, string? Error)> Completion)>();
    try
    {
        while (true)
        {
            foreach (JsonElement operation in abi.Json(abi.rc_events(client)).EnumerateArray())
            {
                int id = operation.GetProperty("id").GetInt32();
                string kind = operation.GetProperty("kind").GetString()!;
                if (kind == "cancel")
                {
                    if (pending.Remove(id, out var resource)) { resource.Cancellation.Cancel(); resource.Cancellation.Dispose(); }
                }
                else
                {
                    var operationCancellation = new CancellationTokenSource();
                    pending.Add(id, (operationCancellation, Execute(abi, operation, http, operationCancellation.Token)));
                }
            }
            int result = abi.Check(abi.rc_result(client));
            if (result != 0) return ResolveOutcome(abi, abi.Json(result));
            if (pending.Count == 0) throw new InvalidOperationException("Pending guest has no host work");
            Task<(int Status, JsonElement Headers, byte[] Body, string? Error)> completed =
                await Task.WhenAny(pending.Values.Select(value => value.Completion)).WaitAsync(cancellation);
            int completedId = pending.First(value => value.Value.Completion == completed).Key;
            var completedResource = pending[completedId];
            pending.Remove(completedId);
            completedResource.Cancellation.Dispose();
            var payload = await completed;
            // Bodies attach to guest buffers at the loop head only; guest calls never run on
            // HTTP or timer completion threads.
            JsonElement reply;
            if (payload.Error is not null)
            {
                reply = JsonSerializer.SerializeToElement(new { error = payload.Error });
            }
            else if (payload.Body is null)
            {
                reply = JsonSerializer.SerializeToElement(new { });
            }
            else
            {
                reply = JsonSerializer.SerializeToElement(new
                {
                    status = payload.Status,
                    headers = payload.Headers,
                    body = abi.WriteBytes(payload.Body)
                });
            }
            Input(abi, reply, buffer => abi.rc_complete(client, completedId, buffer));
        }
    }
    catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
    {
        abi.Check(abi.rc_cancel(client));
        return ResolveOutcome(abi, abi.Json(abi.rc_result(client)));
    }
    catch
    {
        abi.rc_cancel(client);
        int result = abi.rc_result(client);
        if (result > 0) ResolveOutcome(abi, abi.Json(result));
        throw;
    }
    finally
    {
        foreach (var resource in pending.Values) { resource.Cancellation.Cancel(); resource.Cancellation.Dispose(); }
        await Task.WhenAll(pending.Values.Select(value => value.Completion));
    }
}

static async Task<(int Status, JsonElement Headers, byte[] Body, string? Error)> Execute(RcAbi abi, JsonElement operation, HttpClient http, CancellationToken cancellation)
{
    try
    {
        string kind = operation.GetProperty("kind").GetString()!;
        if (kind == "sleep")
        {
            await Task.Delay(TimeSpan.FromMilliseconds(operation.GetProperty("milliseconds").GetDouble()), cancellation);
            return (0, default, null!, null);
        }
        if (kind != "http") throw new InvalidOperationException($"Unknown host operation: {kind}");
        using var request = new HttpRequestMessage(new HttpMethod(operation.GetProperty("method").GetString()!), operation.GetProperty("url").GetString());
        // The request body is read before the first await so the guest call stays on the driver thread.
        request.Content = new ByteArrayContent(abi.Read(operation.GetProperty("body").GetInt32()));
        foreach (JsonElement header in operation.GetProperty("headers").EnumerateArray())
        {
            string name = header[0].GetString()!;
            string value = header[1].GetString()!;
            if (!request.Headers.TryAddWithoutValidation(name, value)) request.Content.Headers.Add(name, value);
        }
        using HttpResponseMessage response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellation);
        await using Stream stream = await response.Content.ReadAsStreamAsync(cancellation);
        using var body = new MemoryStream();
        byte[] chunk = new byte[64 * 1024];
        int count;
        while ((count = await stream.ReadAsync(chunk, cancellation)) != 0)
        {
            if (body.Length + count > 16 * 1024 * 1024) throw new InvalidOperationException("Response exceeds 16 MiB");
            body.Write(chunk, 0, count);
        }
        var headers = JsonSerializer.SerializeToElement(response.Headers.Concat(response.Content.Headers)
            .SelectMany(pair => pair.Value.Select(value => new[] { pair.Key, value })));
        return ((int)response.StatusCode, headers, body.ToArray(), null);
    }
    catch (Exception error) when (error is HttpRequestException or OperationCanceledException or IOException or InvalidOperationException)
    {
        return (0, default, Array.Empty<byte>(), error.Message);
    }
}
