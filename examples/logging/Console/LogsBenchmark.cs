// <copyright file="LogsBenchmark.cs" company="Datadog">
// Unless explicitly stated otherwise all files in this repository are licensed under the Apache 2 License.
// This product includes software developed at Datadog (https://www.datadoghq.com/). Copyright 2025 Datadog, Inc.
// </copyright>

using System;
using System.Runtime.InteropServices;
using System.Text;
using BenchmarkDotNet.Attributes;
using BenchmarkDotNet.Running;

[MemoryDiagnoser(true)]
public class LogBenchmarks
{
    private static IntPtr messagePtr;

    [GlobalSetup]
    public unsafe void Setup()
    {
        string testMessage = "🔥 Benchmarking Rust to C# logging performance! 🔥";
        byte[] utf8Bytes = Encoding.UTF8.GetBytes(testMessage + "\0"); // Add null terminator
        messagePtr = Marshal.AllocHGlobal(utf8Bytes.Length);
        Marshal.Copy(utf8Bytes, 0, messagePtr, utf8Bytes.Length);
    }

    [GlobalCleanup]
    public void Cleanup()
    {
        Marshal.FreeHGlobal(messagePtr);
    }

    [Benchmark]
    public string UsingPtrToStringAnsi()
    {
        return Marshal.PtrToStringAnsi(messagePtr);
    }

    [Benchmark]
    public string UsingUtf8GetString()
    {
        ReadOnlySpan<byte> span;
        unsafe
        {
            byte* rawPtr = (byte*)messagePtr;
            int length = 0;
            while (rawPtr[length] != 0) length++;

            span = new ReadOnlySpan<byte>(rawPtr, length);
        }
        return Encoding.UTF8.GetString(span);
    }
}