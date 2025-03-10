using System;
using System.Runtime.InteropServices;
using BenchmarkDotNet.Running;

class Program
{
    // Delegate for the callback
    private delegate void LogCallback(int level, IntPtr messagePtr);

    // Import Rust functions
    [DllImport("/Users/ganesh.jangir/dd/libdatadog/target/debug/libdata_pipeline_ffi.dylib")]
    private static extern int set_log_callback(LogCallback callback);

    [DllImport("/Users/ganesh.jangir/dd/libdatadog/target/debug/libdata_pipeline_ffi.dylib")]
    private static extern void trigger_logs();

    [DllImport("/Users/ganesh.jangir/dd/libdatadog/target/debug/libdata_pipeline_ffi.dylib")]
    private static extern void free_log_message(IntPtr messagePtr);

    // Log handler that reads directly from Rust memory (zero-copy)
    private static unsafe void LogHandler(int level, IntPtr messagePtr)
    {
        if (messagePtr == IntPtr.Zero)
        {
            Console.WriteLine("⚠️ Received null message pointer!");
            return;
        }

        // Convert raw pointer to a ReadOnlySpan<byte>
        byte* rawPtr = (byte*)messagePtr;
        int length = 0;
        while (rawPtr[length] != 0) length++; // Find null terminator

        ReadOnlySpan<byte> byteSpan = new ReadOnlySpan<byte>(rawPtr, length);

        // Convert byte span to UTF-8 string (without extra allocations)
        string message = System.Text.Encoding.UTF8.GetString(byteSpan);

        // Print log message
        switch (level)
        {
            case 3:
                Console.Write("[ERROR] ");
                Console.WriteLine(message);
                break;
            case 2:
                Console.Write("[WARN] ");
                Console.WriteLine(message);
                break;
            default:
                Console.Write("[INFO] ");
                Console.WriteLine(message);
                break;
        }
    }

    static void Main()
    {
        // // Register log callback
        // int result = set_log_callback(LogHandler);
        // Console.WriteLine("set_log_callback result: " + result);
        //
        // // Trigger Rust logs
        // trigger_logs();

        var summary = BenchmarkRunner.Run<LogBenchmarks>();

    }
}
