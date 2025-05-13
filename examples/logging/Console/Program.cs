using System.Runtime.InteropServices;
using System.Text;
using Serilog;
using Serilog.Events;

namespace Console
{
    // Add this near the top of the file, inside the namespace but outside any class
    internal static class SpanExtensions
    {
        public static T[] ToArray<T>(this Span<T> span)
        {
            return span.ToArray();
        }
    }

    // Core FFI structures that match Rust definitions
    [StructLayout(LayoutKind.Sequential)]
    internal readonly struct FFIVec
    {
        internal readonly IntPtr Data;
        internal readonly nuint Length;
        internal readonly nuint Capacity;

        public Span<T> AsSpan<T>() where T : unmanaged
        {
            if (Data == IntPtr.Zero)
                return Span<T>.Empty;

            unsafe
            {
                return new Span<T>((void*)Data, (int)Length);
            }
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    internal readonly struct Error
    {
        internal readonly FFIVec Message;

        // Similar to Rust's From trait
        public Exception ToException()
        {
            var messageBytes = Message.AsSpan<byte>();
            var message = Encoding.UTF8.GetString(messageBytes);
            return new Exception(message);
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    internal readonly struct CharSlice
    {
        internal readonly IntPtr Ptr;
        internal readonly nuint Len;

        // Implement ToString for clean conversion
        public override string ToString()
        {
            if (Ptr == IntPtr.Zero || Len == 0) return string.Empty;

            unsafe
            {
                return Encoding.UTF8.GetString((byte*)Ptr, (int)Len);
            }
        }
    }

    [StructLayout(LayoutKind.Sequential)]
    internal readonly struct LogField
    {
        internal readonly CharSlice Key;
        internal readonly CharSlice Value;

        public readonly (string Key, string Value) Deconstruct() =>
            (Key.ToString(), Value.ToString());
    }

    [StructLayout(LayoutKind.Sequential)]
    internal readonly struct LogEvent
    {
        internal readonly LogLevel Level;
        internal readonly CharSlice Message;
        internal readonly FFIVec Fields;

        public readonly Span<LogField> GetFields() => Fields.AsSpan<LogField>();
    }

    // Safe handle for managing FFI resources
    internal sealed class ErrorHandle : SafeHandle
    {
        public ErrorHandle() : base(IntPtr.Zero, true) { }

        public override bool IsInvalid => handle == IntPtr.Zero;

        protected override bool ReleaseHandle()
        {
            if (!IsInvalid)
            {
                ddog_Error_drop(handle);
            }
            return true;
        }

        [DllImport(NativeMethods.DllPath)]
        private static extern void ddog_Error_drop(IntPtr error);
    }

    // Enums
    public enum LogLevel
    {
        Debug = 0,
        Info = 1,
        Warn = 2,
        Error = 3,
        Trace = 4
    }

    // Native methods wrapper
    internal static class NativeMethods
    {
        #if DEBUG
            internal const string DllPath = "../../../../../../target/debug/libdata_pipeline_ffi.dylib";
        #else
            internal const string DllPath = "libdata_pipeline_ffi.dylib";
        #endif

        [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
        internal delegate void LogCallback(LogEvent logEvent);

        [DllImport(DllPath, CallingConvention = CallingConvention.Cdecl)]
        internal static extern ErrorHandle ddog_logger_init(LogLevel level, LogCallback callback);

        [DllImport(DllPath, CallingConvention = CallingConvention.Cdecl)]
        internal static extern ErrorHandle ddog_logger_set_max_log_level(LogLevel level);

        [DllImport(DllPath, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void trigger_logs();

        [DllImport(DllPath, CallingConvention = CallingConvention.Cdecl)]
        internal static extern void trigger_logs_with_args();
    }

    // Main logger implementation
    public sealed class DatadogLogger : ILogger, IDisposable
    {
        private static readonly Lazy<DatadogLogger> LazyInstance = new(() => new DatadogLogger());
        private const string InitializationMessage = "Logger initialized with {LogLevel} level";
        private const string LogLevelChangeMessage = "Log level set to {LogLevel}";
        private bool _isDisposed;
        private LogLevel _currentLevel = LogLevel.Debug;
        private Serilog.ILogger _logger;

        private DatadogLogger() { }

        public static DatadogLogger Instance => LazyInstance.Value;

        public void Initialize(LogLevel level = LogLevel.Debug)
        {
            ThrowIfDisposed();

            // Configure Serilog
            _logger = new LoggerConfiguration()
                .MinimumLevel.Is(ToSerilogLevel(level))
                .WriteTo.Console(
                    outputTemplate: "[{Level:u3}] {Message:lj}{NewLine}{Properties}{NewLine}{Exception}")
                .CreateLogger();

            using var result = NativeMethods.ddog_logger_init(level, LogHandler);
            ThrowIfError(result);
            _currentLevel = level;
        }

        public void SetMaxLogLevel(LogLevel level)
        {
            ThrowIfDisposed();
            using var result = NativeMethods.ddog_logger_set_max_log_level(level);
            ThrowIfError(result);
            _currentLevel = level;

            // Reconfigure Serilog with new level
            _logger = new LoggerConfiguration()
                .MinimumLevel.Is(ToSerilogLevel(level))
                .WriteTo.Console(
                    outputTemplate: "[{Level:u3}] {Message:lj}{NewLine}{Properties}{NewLine}{Exception}")
                .CreateLogger();
        }

        public bool IsEnabled(LogLevel level) => level >= _currentLevel;

        public void Log(LogLevel level, string message, params (string Key, object Value)[] fields)
        {
            if (!IsEnabled(level))
                return;

            var serilogLevel = ToSerilogLevel(level);

            // Create a template with named properties
            var propertyValues = fields.Select(f => f.Value).ToArray();
            var template = CreateTemplateWithProperties(message, fields);

            _logger.Write(serilogLevel, template, propertyValues);
        }

        public void LogError(string message, Exception exception = null, params (string Key, object Value)[] fields)
        {
            if (!IsEnabled(LogLevel.Error))
                return;

            // Create a template with named properties
            var propertyValues = fields.Select(f => f.Value).ToArray();
            var template = CreateTemplateWithProperties(message, fields);

            _logger.Error(exception, template, propertyValues);
        }

        public void LogWarning(string message, params (string Key, object Value)[] fields) =>
            Log(LogLevel.Warn, message, fields);

        public void LogInfo(string message, params (string Key, object Value)[] fields) =>
            Log(LogLevel.Info, message, fields);

        public void LogDebug(string message, params (string Key, object Value)[] fields) =>
            Log(LogLevel.Debug, message, fields);

        public void LogTrace(string message, params (string Key, object Value)[] fields) =>
            Log(LogLevel.Trace, message, fields);

        private static LogEventLevel ToSerilogLevel(LogLevel level) => level switch
        {
            LogLevel.Error => LogEventLevel.Error,
            LogLevel.Warn => LogEventLevel.Warning,
            LogLevel.Info => LogEventLevel.Information,
            LogLevel.Debug => LogEventLevel.Debug,
            LogLevel.Trace => LogEventLevel.Verbose,
            _ => LogEventLevel.Information
        };

        private void ThrowIfDisposed()
        {
            if (_isDisposed)
            {
                throw new ObjectDisposedException(nameof(DatadogLogger));
            }
        }

        private static void ThrowIfError(ErrorHandle handle)
        {
            if (!handle.IsInvalid)
            {
                var error = Marshal.PtrToStructure<Error>(handle.DangerousGetHandle());
                throw error.ToException();
            }
        }

        private static void LogHandler(LogEvent logEvent)
        {
            var logger = Instance;
            var fields = logEvent.GetFields()
                .ToArray()
                .Select(f => (f.Key.ToString(), (object)f.Value.ToString()))
                .ToArray();

            logger.Log(logEvent.Level, logEvent.Message.ToString(), fields);
        }

        // Helper method to create message template with properties
        private static string CreateTemplateWithProperties(string message, (string Key, object Value)[] fields)
        {
            if (fields.Length == 0)
                return message;

            var properties = string.Join(" ", fields.Select(f => $"{f.Key}={{{f.Key}}}"));
            return $"{message} {properties}";
        }

        public void Dispose()
        {
            if (_isDisposed)
                return;

            (_logger as IDisposable)?.Dispose();
            _isDisposed = true;
            GC.SuppressFinalize(this);
        }
    }

    // Example usage
    class Program
    {
        static async Task Main()
        {
            await RunLoggerDemo();
        }

        private static async Task RunLoggerDemo()
        {
            using var logger = DatadogLogger.Instance;
            logger.Initialize(LogLevel.Debug);

            await RunTestScenario("Testing built-in log messages:", () =>
            {
                logger.LogInfo("Starting built-in log test");
                NativeMethods.trigger_logs();
                return Task.CompletedTask;
            });

            await RunTestScenario("Testing logs with arguments:", () =>
            {
                logger.LogInfo("Starting argument log test");
                NativeMethods.trigger_logs_with_args();
                return Task.CompletedTask;
            });

            await RunTestScenario("Changing log level to Error:", () =>
            {
                logger.LogInfo("Changing to error level");
                logger.SetMaxLogLevel(LogLevel.Error);
                NativeMethods.trigger_logs();
                return Task.CompletedTask;
            });

            await RunTestScenario("Changing log level back to Debug:", () =>
            {
                logger.LogInfo("Changing back to debug level");
                logger.SetMaxLogLevel(LogLevel.Debug);
                NativeMethods.trigger_logs();
                return Task.CompletedTask;
            });
        }

        private static async Task RunTestScenario(string description, Func<Task> action)
        {
            System.Console.WriteLine($"\n{description}");
            await action();
        }
    }

    public interface ILogger
    {
        void Log(LogLevel level, string message, params (string Key, object Value)[] fields);
        void LogError(string message, Exception exception = null, params (string Key, object Value)[] fields);
        void LogWarning(string message, params (string Key, object Value)[] fields);
        void LogInfo(string message, params (string Key, object Value)[] fields);
        void LogDebug(string message, params (string Key, object Value)[] fields);
        void LogTrace(string message, params (string Key, object Value)[] fields);
        bool IsEnabled(LogLevel level);
    }
}
