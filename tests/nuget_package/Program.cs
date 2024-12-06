// See https://aka.ms/new-console-template for more information
using System.Runtime.InteropServices;

var url = new CharSlice("http://localhost:8126");
var tracerVersion = new CharSlice("1.0.0");
var language = new CharSlice(".NET");
var languageVersion = new CharSlice("5.0.0");
var languageInterpreter = new CharSlice(".NET");
var hostname = new CharSlice("localhost");
var env = new CharSlice("development");
var service = new CharSlice("dotnet-test");
var serviceVersion = new CharSlice("1.0.0");

var handle = IntPtr.Zero;

Console.WriteLine("Creating exporter");
var error = Native.ddog_trace_exporter_new(
    outHandle: ref handle,
    url: url,
    tracerVersion: tracerVersion,
    language: language,
    languageVersion: languageVersion,
    languageInterpreter: languageInterpreter,
    hostname: hostname,
    env: env,
    version: serviceVersion,
    service: service,
    inputFormat: TraceExporterInputFormat.V04,
    outputFormat: TraceExporterOutputFormat.V04,
    computeStats: false,
    agentResponseCallback: (IntPtr chars) =>
    {
        var response = Marshal.PtrToStringUni(chars);
        Console.WriteLine(response);
    }
);

if (error.Tag == ErrorTag.Some)
{
    Console.WriteLine("Error creating exporter");
    Console.WriteLine(Marshal.PtrToStringUni(error.Message.Ptr));
    Native.ddog_MaybeError_drop(error);
    return;
}

if (handle == IntPtr.Zero)
{
    Console.WriteLine("Error creating exporter");
    return;
}

Console.WriteLine("Exporter created");

Console.WriteLine("Freeing exporter");
Native.ddog_trace_exporter_free(handle);
Console.WriteLine("Exporter freed");
Console.WriteLine("Done");

internal enum TraceExporterInputFormat
{
    Proxy = 0,
    V04 = 1,
}

internal enum TraceExporterOutputFormat
{
    V04 = 0,
    V07 = 1,
}

internal delegate void AgentResponseCallback(IntPtr response);

internal static class Native
{
    private const string DllName = "datadog_profiling_ffi";

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern MaybeError ddog_trace_exporter_new(
        ref IntPtr outHandle,
        CharSlice url,
        CharSlice tracerVersion,
        CharSlice language,
        CharSlice languageVersion,
        CharSlice languageInterpreter,
        CharSlice hostname,
        CharSlice env,
        CharSlice version,
        CharSlice service,
        TraceExporterInputFormat inputFormat,
        TraceExporterOutputFormat outputFormat,
        bool computeStats,
        AgentResponseCallback agentResponseCallback);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void ddog_MaybeError_drop(MaybeError error);

    [DllImport(DllName, CallingConvention = CallingConvention.Cdecl)]
    internal static extern void ddog_trace_exporter_free(IntPtr handle);
}

[StructLayout(LayoutKind.Sequential)]
internal struct CharSlice
{
    internal IntPtr Ptr;
    internal UIntPtr Len;

    internal CharSlice(string str)
    {
        var bytes = System.Text.Encoding.UTF8.GetBytes(str);
        Ptr = Marshal.AllocHGlobal(bytes.Length);
        Marshal.Copy(bytes, 0, Ptr, bytes.Length);
        Len = (UIntPtr)bytes.Length;
    }
}

internal enum ErrorTag
{
    Some = 0,
    None = 1,
}

[StructLayout(LayoutKind.Sequential)]
internal struct MaybeError
{
    internal ErrorTag Tag;
    internal CharSlice Message;
}
