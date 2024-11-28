# libdatadog

## About

This package is binary distribution of [libdatadog](https://github.com/DataDog/libdatadog). It supports both native (C/C++) and .NET projects.

## Getting started

### Native (C/C++) projects

For Native projects, `libdatadog` supports both static and dynamic linking. The dynamic linking is the default option. To use the static linking, you need to disable the dynamic linking by setting the msbuild property `LibDatadogDynamicLink` to `false`. Both `debug` and `release` congfiguration libraries are provided to allow better debugging experience.

Depending on the target platform, binaries are compied to project output directory.

### .NET projects

For .NET projects, `libdatadog` supports only dynamic linking targetting .NET Standard 2.0.

Depending on the target platform, binaries are copied to project output directory under `runtimes` directory as per [.NET conventions](https://learn.microsoft.com/en-us/nuget/create-packages/native-files-in-net-packages#native-assets) (`runtimes/{RID}/native`). 

> [!IMPORTANT]  
> `nuget` fails to resolve the shared library if same directory is not used in the nuget package and the project output directory. Hence, only `release` configuration is supported for .NET projects.

## Package content

```
libdatadog.14.3.1-ci.771.90/
├── build
│   ├── native
│   │   ├── lib
│   │   │   ├── x64
│   │   │   │   ├── debug
│   │   │   │   │   ├── libdatadog win-x64 shared library and pdb (debug)
│   │   │   │   │   └── static
│   │   │   │   │       └── libdatadog win-x64 static library (debug)
│   │   │   │   └── release
│   │   │   │       ├── libdatadog win-x64 shared library and pdb (release)
│   │   │   │       └── static
│   │   │   │           └── libdatadog win-x64 static library (release)
│   │   │   └── x86
│   │   │       ├── debug
│   │   │       │   ├── libdatadog win-x86 shared library and pdb (debug)
│   │   │       │   └── static
│   │   │       │       └── libdatadog win-x86 static library (debug)
│   │   │       └── release
│   │   │           ├── libdatadog win-x86 shared library and pdb (release)
│   │   │           └── static
│   │   │               └── libdatadog win-x86 static library (release)
│   │   └── libdatadog.props
│   └── netstandard2.0
│       └── libdatadog.props
├── include
│   └── native
│       └── datadog
│           ├── C headers
└── runtimes
    ├── win-x64
    │   └── native
    │       └── libdatadog win-x64 shared library and pdb
    └── win-x86
        └── native
            └── libdatadog win-x86 shared library and pdb 
```

## License

Apache-2.0