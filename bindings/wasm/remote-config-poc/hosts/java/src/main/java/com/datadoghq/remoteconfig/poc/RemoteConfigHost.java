// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

package com.datadoghq.remoteconfig.poc;

import run.endive.runtime.ImportValues;
import run.endive.runtime.Instance;
import run.endive.wasi.WasiOptions;
import run.endive.wasi.WasiPreview1;

/**
 * Loads the shared remote-config reactor for hosting: the module AOT-compiled into the jar ({@code
 * RemoteConfigMachine} classes plus the stripped {@code RemoteConfig.meta}), a WASI Preview 1
 * instance with inherited clocks and randomness, and the generated {@link RcAbi} bindings over the
 * rc_ exports.
 *
 * <p>The host owns the Endive instance lifetime; {@link #close()} releases WASI. Callers keep the
 * host open as long as any client created from {@link #abi()} is in use.
 */
public final class RemoteConfigHost implements AutoCloseable {

  private final WasiPreview1 wasi;
  private final Instance instance;
  private final RcAbi abi;

  private RemoteConfigHost(WasiPreview1 wasi, Instance instance, RcAbi abi) {
    this.wasi = wasi;
    this.instance = instance;
    this.abi = abi;
  }

  /** Loads the reactor: AOT module, WASI preview 1 (inherited clocks/randomness), and ABI. */
  public static RemoteConfigHost load() {
    RemoteConfig compiled = new RemoteConfig();
    WasiOptions wasiOptions = WasiOptions.builder().inheritSystemStreams().build();
    WasiPreview1 wasi = WasiPreview1.builder().withOptions(wasiOptions).build();
    try {
      ImportValues imports = ImportValues.builder().addFunction(wasi.toHostFunctions()).build();
      Instance instance =
          Instance.builder(compiled.wasmModule())
              .withStart(false)
              .withImportValues(imports)
              .withMachineFactory(compiled.machineFactory())
              .build();
      // Reactors may export _initialize; data segments are already applied at instantiation.
      try {
        instance.export("_initialize").apply();
      } catch (run.endive.wasm.InvalidException absent) {
        // The shared reactor applies its data segments on instantiation; a missing export is fine.
      }
      return new RemoteConfigHost(wasi, instance, new RcAbi(instance));
    } catch (RuntimeException error) {
      wasi.close();
      throw error;
    }
  }

  /** The ABI bindings over the rc_ exports of the loaded reactor. */
  public RcAbi abi() {
    return abi;
  }

  /** The Endive instance of the loaded reactor. */
  public Instance instance() {
    return instance;
  }

  @Override
  public void close() {
    wasi.close();
  }
}
