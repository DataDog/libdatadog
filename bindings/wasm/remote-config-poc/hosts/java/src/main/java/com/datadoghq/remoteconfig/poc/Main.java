// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package com.datadoghq.remoteconfig.poc;

import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Base64;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ScheduledThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import org.json.JSONArray;
import org.json.JSONObject;
import run.endive.runtime.CompiledModule;
import run.endive.runtime.ImportValues;
import run.endive.runtime.Instance;
import run.endive.wasi.WasiOptions;
import run.endive.wasi.WasiPreview1;

/**
 * Java host for the shared remote-config WASM reactor.
 *
 * <p>Mirrors {@code hosts/dotnet/Program.cs}: loads the wasm32-wasip1 reactor with Endive, wires
 * WASI preview1, runs {@code _initialize}, then drives the JSON event bridge over the rc_ ABI.
 * Guest calls never overlap: HTTP completions are only dispatched from the poll loop head.
 */
public final class Main {

    private static final int MAX_BODY = 16 * 1024 * 1024;
    private static final Object CANCEL = new Object();

    private Main() {}

    /** One pending host operation: either an in-flight HTTP request or a timer. */
    private static final class Pending {
        final int id;
        final CompletableFuture<JSONObject> completion;
        final Runnable canceller;

        Pending(int id, CompletableFuture<JSONObject> completion, Runnable canceller) {
            this.id = id;
            this.completion = completion;
            this.canceller = canceller;
        }
    }

    /** A guest call taking one input buffer, mirroring the {@code Input} helper of the .NET host. */
    @FunctionalInterface
    private interface GuestCall {
        int apply(int buffer);
    }

    public static void main(String[] args) throws Exception {
        // The module is compiled ahead of time into the RemoteConfigMachine classes; the wasm
        // path is still accepted as the first argument to keep the host CLI uniform with the
        // Node and .NET hosts, but the guest executed here is the one AOT-compiled at build time.
        CompiledModule compiled = new RemoteConfig();
        WasiOptions wasiOptions = WasiOptions.builder().inheritSystem().build();
        try (WasiPreview1 wasi = WasiPreview1.builder().withOptions(wasiOptions).build()) {
            ImportValues imports =
                    ImportValues.builder().addFunction(wasi.toHostFunctions()).build();
            Instance instance = Instance.builder(compiled.wasmModule())
                    .withStart(false)
                    .withImportValues(imports)
                    .withMachineFactory(compiled.machineFactory())
                    .build();
            runInitialize(instance);
            RcAbi abi = new RcAbi(instance);
            HttpClient http = HttpClient.newHttpClient();
            JSONObject scenario = new JSONObject(Files.readString(Path.of(args[2]), StandardCharsets.UTF_8));
            JSONObject options = scenario.getJSONObject("options");
            options.put("url", args[1]);
            options.put("language", "java");
            int client = input(abi, options, buffer -> abi.create(buffer));
            try {
                if (args.length > 3 && args[3].equals("--cancel-stdin")) {
                    LinkedBlockingQueue<Object> wakeups = new LinkedBlockingQueue<>();
                    CompletableFuture<JSONObject> pending =
                            CompletableFuture.supplyAsync(() -> poll(abi, client, http, wakeups));
                    BufferedReader stdin = new BufferedReader(new InputStreamReader(System.in, StandardCharsets.UTF_8));
                    if (!"cancel".equals(stdin.readLine())) {
                        throw new IllegalStateException("Expected cancel command");
                    }
                    wakeups.add(CANCEL);
                    System.out.println(pending.join());
                    abi.check(abi.poll(client));
                    abi.check(abi.cancel(client));
                    abi.json(abi.result(client));
                    System.out.println("{\"restarted\":true}");
                } else {
                    for (Object entry : scenario.getJSONArray("actions")) {
                        JSONObject action = (JSONObject) entry;
                        String kind = action.getString("kind");
                        JSONObject outcome;
                        if (kind.equals("ack")) {
                            input(abi, action.getJSONObject("value"), buffer -> abi.ack(client, buffer));
                            outcome = new JSONObject().put("acknowledged", true);
                        } else if (kind.equals("cancel") || kind.equals("timeout")) {
                            outcome = cancelOrTimeout(abi, client, action, kind);
                        } else if (kind.equals("fixture")) {
                            outcome = fixture(abi, client, action);
                        } else if (kind.equals("poll")) {
                            outcome = poll(abi, client, http, new LinkedBlockingQueue<>());
                        } else {
                            throw new IllegalStateException("Unknown scenario action: " + kind);
                        }
                        System.out.println(outcome);
                    }
                }
            } finally {
                abi.check(abi.close(client));
            }
        }
        System.exit(0);
    }

    /** Reactors may export {@code _initialize}; data segments are already applied at instantiation. */
    private static void runInitialize(Instance instance) {
        try {
            instance.export("_initialize").apply();
        } catch (run.endive.wasm.InvalidException absent) {
            // The shared reactor applies its data segments on instantiation; a missing export is fine.
        }
    }

    /** Drives the {@code cancel} and {@code timeout} scenario actions without a real network. */
    private static JSONObject cancelOrTimeout(RcAbi abi, int client, JSONObject action, String kind) {
        abi.check(abi.poll(client));
        String wanted = kind.equals("timeout") ? "sleep" : "http";
        JSONObject operation = find(abi.jsonArray(abi.check(abi.events(client))), wanted);
        if (operation == null) throw new IllegalStateException("Expected pending operation");
        int operationId = operation.getInt("id");
        if (kind.equals("cancel")) {
            abi.check(abi.cancel(client));
        } else {
            input(abi, new JSONObject(), buffer -> abi.complete(client, operationId, buffer));
        }
        JSONObject outcome = abi.json(abi.result(client));
        int reply = abi.write(new JSONObject().put("status", 200).put("body", "e30="));
        try {
            if (abi.complete(client, operationId, reply) != -1) {
                throw new IllegalStateException("Late completion accepted");
            }
        } finally {
            abi.check(abi.free(reply));
        }
        if (find(abi.jsonArray(abi.check(abi.events(client))), "cancel") == null) {
            throw new IllegalStateException("No host cancellation");
        }
        return outcome;
    }

    /** Serves the agentless fixture requests directly through the ABI, never through HTTP. */
    private static JSONObject fixture(RcAbi abi, int client, JSONObject action) {
        abi.check(abi.poll(client));
        JSONArray requests = new JSONArray();
        JSONObject outcome = null;
        for (int step = 0; step < 32 && outcome == null; step++) {
            JSONArray events = abi.jsonArray(abi.check(abi.events(client)));
            boolean hasHttp = find(events, "http") != null;
            for (Object entry : events) {
                JSONObject operation = (JSONObject) entry;
                String kind = operation.getString("kind");
                if (kind.equals("sleep") && !hasHttp) {
                    int id = operation.getInt("id");
                    input(abi, new JSONObject(), buffer -> abi.complete(client, id, buffer));
                }
                if (!kind.equals("http")) continue;
                requests.put(operation);
                int id = operation.getInt("id");
                String path = URI.create(operation.getString("url")).getPath();
                String body = path.equals("/api/v0.1/org") ? "" : action.getString("body");
                input(abi, new JSONObject().put("status", 200).put("body", body),
                        buffer -> abi.complete(client, id, buffer));
            }
            int result = abi.check(abi.result(client));
            if (result > 0) {
                outcome = new JSONObject()
                        .put("outcome", abi.json(result))
                        .put("requests", requests);
            }
        }
        if (outcome == null) throw new IllegalStateException("Fixture did not complete");
        return outcome;
    }

    /**
     * The poll loop: drains host events, dispatches HTTP and timers, and completes one operation
     * at a time from the loop head so guest calls never overlap. Mirrors the .NET {@code Poll}.
     */
    static JSONObject poll(RcAbi abi, int client, HttpClient http, LinkedBlockingQueue<Object> wakeups) {
        ScheduledThreadPoolExecutor scheduler = new ScheduledThreadPoolExecutor(1, runnable -> {
            Thread thread = new Thread(runnable, "remote-config-timers");
            thread.setDaemon(true);
            return thread;
        });
        Map<Integer, Pending> pending = new LinkedHashMap<>();
        try {
            abi.check(abi.poll(client));
            while (true) {
                for (Object entry : abi.jsonArray(abi.check(abi.events(client)))) {
                    JSONObject operation = (JSONObject) entry;
                    int id = operation.getInt("id");
                    String kind = operation.getString("kind");
                    if (kind.equals("cancel")) {
                        Pending resource = pending.remove(id);
                        if (resource != null) resource.canceller.run();
                    } else {
                        Pending resource = dispatch(operation, http, scheduler, wakeups);
                        pending.put(id, resource);
                    }
                }
                int result = abi.check(abi.result(client));
                if (result != 0) return abi.json(result);
                if (pending.isEmpty()) throw new IllegalStateException("Pending guest has no host work");
                Object wake;
                try {
                    wake = wakeups.take();
                } catch (InterruptedException interrupted) {
                    Thread.currentThread().interrupt();
                    throw new IllegalStateException("Poll dispatcher interrupted", interrupted);
                }
                if (wake == CANCEL) {
                    abi.check(abi.cancel(client));
                    return abi.json(abi.result(client));
                }
                Pending completed = pending.remove((Integer) wake);
                if (completed == null) continue; // Stale wake for an operation the guest cancelled.
                JSONObject reply = completed.completion.join();
                int operationId = completed.id;
                input(abi, reply, buffer -> abi.complete(client, operationId, buffer));
            }
        } catch (RuntimeException error) {
            // An adapter failure must not leave a guest future retaining client state.
            abi.cancel(client);
            int result = abi.result(client);
            if (result > 0) abi.read(result);
            throw error;
        } finally {
            scheduler.shutdownNow();
            for (Pending resource : pending.values()) resource.canceller.run();
        }
    }

    /** Starts the host work described by one event and returns its handle. */
    private static Pending dispatch(JSONObject operation, HttpClient http,
            ScheduledThreadPoolExecutor scheduler, LinkedBlockingQueue<Object> wakeups) {
        int id = operation.getInt("id");
        String kind = operation.getString("kind");
        CompletableFuture<JSONObject> completion = new CompletableFuture<>();
        if (kind.equals("sleep")) {
            long milliseconds = (long) operation.getDouble("milliseconds");
            ScheduledFuture<?> timer = scheduler.schedule(
                    () -> completion.complete(new JSONObject()), milliseconds, TimeUnit.MILLISECONDS);
            return new Pending(id, completion, () -> timer.cancel(false));
        }
        if (!kind.equals("http")) throw new IllegalStateException("Unknown host operation: " + kind);

        HttpRequest.Builder request = HttpRequest.newBuilder(URI.create(operation.getString("url")));
        byte[] body = Base64.getDecoder().decode(operation.getString("body"));
        request.method(operation.getString("method"), HttpRequest.BodyPublishers.ofByteArray(body));
        for (Object entry : operation.getJSONArray("headers")) {
            JSONArray header = (JSONArray) entry;
            request.header(header.getString(0), header.getString(1));
        }
        CompletableFuture<HttpResponse<byte[]>> sent =
                http.sendAsync(request.build(), HttpResponse.BodyHandlers.ofByteArray());
        sent.whenComplete((response, error) -> wakeups.add(id));
        return new Pending(id, sent.handle(Main::reply), () -> sent.cancel(true));
    }

    /** Converts an HTTP response or failure into the bridge reply JSON. */
    private static JSONObject reply(HttpResponse<byte[]> response, Throwable error) {
        if (error != null) {
            Throwable cause = error.getCause() != null ? error.getCause() : error;
            return new JSONObject().put("error", cause.getMessage());
        }
        byte[] body = response.body();
        if (body.length > MAX_BODY) {
            return new JSONObject().put("error", "Response exceeds 16 MiB");
        }
        JSONArray headers = new JSONArray();
        for (Map.Entry<String, java.util.List<String>> entry : response.headers().map().entrySet()) {
            for (String value : entry.getValue()) {
                headers.put(new JSONArray().put(entry.getKey()).put(value));
            }
        }
        return new JSONObject()
                .put("status", response.statusCode())
                .put("headers", headers)
                .put("body", Base64.getEncoder().encodeToString(body));
    }

    private static JSONObject find(JSONArray events, String kind) {
        for (Object entry : events) {
            JSONObject operation = (JSONObject) entry;
            if (operation.getString("kind").equals(kind)) return operation;
        }
        return null;
    }

    private static int input(RcAbi abi, Object json, GuestCall operation) {
        int buffer = abi.write(json);
        try {
            return abi.check(operation.apply(buffer));
        } finally {
            abi.check(abi.free(buffer));
        }
    }
}
