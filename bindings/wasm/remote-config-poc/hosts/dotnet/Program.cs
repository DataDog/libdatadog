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
        abi.Json(abi.rc_result(client));
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
                int reply = abi.Write(JsonSerializer.SerializeToElement(new { status = 200, body = "e30=" }));
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
                        requests.Add(operation.Clone());
                        string body = new Uri(operation.GetProperty("url").GetString()!).AbsolutePath == "/api/v0.1/org"
                            ? "" : action["body"]!.GetValue<string>();
                        Input(abi, JsonSerializer.SerializeToElement(new { status = 200, body }), buffer =>
                            abi.rc_complete(client, operation.GetProperty("id").GetInt32(), buffer));
                    }
                    int buffer = abi.Check(abi.rc_result(client));
                    if (buffer > 0) result = abi.Json(buffer);
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

static async Task<JsonElement> Poll(RcAbi abi, int client, HttpClient http, CancellationToken cancellation = default)
{
    abi.Check(abi.rc_poll(client));
    var pending = new Dictionary<int, (CancellationTokenSource Cancellation, Task<JsonElement> Completion)>();
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
                    pending.Add(id, (operationCancellation, Execute(operation, http, operationCancellation.Token)));
                }
            }
            int result = abi.Check(abi.rc_result(client));
            if (result != 0) return abi.Json(result);
            if (pending.Count == 0) throw new InvalidOperationException("Pending guest has no host work");
            Task<JsonElement> completed = await Task.WhenAny(pending.Values.Select(value => value.Completion)).WaitAsync(cancellation);
            int completedId = pending.First(value => value.Value.Completion == completed).Key;
            var completedResource = pending[completedId];
            pending.Remove(completedId);
            completedResource.Cancellation.Dispose();
            JsonElement reply = await completed;
            Input(abi, reply, buffer => abi.rc_complete(client, completedId, buffer));
        }
    }
    catch (OperationCanceledException) when (cancellation.IsCancellationRequested)
    {
        abi.Check(abi.rc_cancel(client));
        return abi.Json(abi.rc_result(client));
    }
    catch
    {
        abi.rc_cancel(client);
        int result = abi.rc_result(client);
        if (result > 0) abi.Read(result);
        throw;
    }
    finally
    {
        foreach (var resource in pending.Values) { resource.Cancellation.Cancel(); resource.Cancellation.Dispose(); }
        await Task.WhenAll(pending.Values.Select(value => value.Completion));
    }
}

static async Task<JsonElement> Execute(JsonElement operation, HttpClient http, CancellationToken cancellation)
{
    try
    {
        string kind = operation.GetProperty("kind").GetString()!;
        if (kind == "sleep")
        {
            await Task.Delay(TimeSpan.FromMilliseconds(operation.GetProperty("milliseconds").GetDouble()), cancellation);
            return JsonSerializer.SerializeToElement(new { });
        }
        if (kind != "http") throw new InvalidOperationException($"Unknown host operation: {kind}");
        using var request = new HttpRequestMessage(new HttpMethod(operation.GetProperty("method").GetString()!), operation.GetProperty("url").GetString());
        request.Content = new ByteArrayContent(Convert.FromBase64String(operation.GetProperty("body").GetString()!));
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
        var headers = response.Headers.Concat(response.Content.Headers).SelectMany(pair => pair.Value.Select(value => new[] { pair.Key, value }));
        return JsonSerializer.SerializeToElement(new { status = (int)response.StatusCode, headers, body = Convert.ToBase64String(body.ToArray()) });
    }
    catch (Exception error) when (error is HttpRequestException or OperationCanceledException or IOException or InvalidOperationException)
    {
        return JsonSerializer.SerializeToElement(new { error = error.Message });
    }
}
