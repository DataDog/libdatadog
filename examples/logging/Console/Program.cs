using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Linq;
using System.Globalization;
using System.Collections.Generic;

namespace DatadogLogger
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
        private const string InitializationMessage = "Logger initialized with {0} level";
        private const string LogLevelChangeMessage = "Log level set to {0}";
        private bool _isDisposed;
        private LogLevel _currentLevel = LogLevel.Debug;

        private DatadogLogger() { }

        public static DatadogLogger Instance => LazyInstance.Value;

        public void Initialize(LogLevel level = LogLevel.Debug)
        {
            ThrowIfDisposed();
            using var result = NativeMethods.ddog_logger_init(level, LogHandler);
            ThrowIfError(result);
            _currentLevel = level;
            Console.WriteLine(InitializationMessage, level);
        }

        public void SetMaxLogLevel(LogLevel level)
        {
            ThrowIfDisposed();
            using var result = NativeMethods.ddog_logger_set_max_log_level(level);
            ThrowIfError(result);
            _currentLevel = level;
            Console.WriteLine(LogLevelChangeMessage, level);
        }

        public bool IsEnabled(LogLevel level) => level >= _currentLevel;

        public void Log(LogLevel level, string message, params (string Key, object Value)[] fields)
        {
            if (!IsEnabled(level))
                return;

            var structuredMessage = FormatStructuredMessage(message, fields);
            WriteToConsole(level, structuredMessage);
        }

        public void LogError(string message, Exception exception = null, params (string Key, object Value)[] fields)
        {
            if (!IsEnabled(LogLevel.Error))
                return;

            var allFields = new List<(string Key, object Value)>(fields);
            if (exception != null)
            {
                allFields.Add(("exception_type", exception.GetType().Name));
                allFields.Add(("exception_message", exception.Message));
                allFields.Add(("stack_trace", exception.StackTrace));
            }

            Log(LogLevel.Error, message, allFields.ToArray());
        }

        public void LogWarning(string message, params (string Key, object Value)[] fields) => 
            Log(LogLevel.Warn, message, fields);

        public void LogInfo(string message, params (string Key, object Value)[] fields) => 
            Log(LogLevel.Info, message, fields);

        public void LogDebug(string message, params (string Key, object Value)[] fields) => 
            Log(LogLevel.Debug, message, fields);

        public void LogTrace(string message, params (string Key, object Value)[] fields) => 
            Log(LogLevel.Trace, message, fields);

        private void WriteToConsole(LogLevel level, string message)
        {
            var originalColor = Console.ForegroundColor;
            try
            {
                Console.ForegroundColor = GetColorForLevel(level);
                Console.Write($"[{level}] ");
                Console.WriteLine(message);
            }
            finally
            {
                Console.ForegroundColor = originalColor;
            }
        }

        private static string FormatStructuredMessage(string message, (string Key, object Value)[] fields)
        {
            var builder = new StringBuilder();
            builder.Append(message);

            if (fields.Length > 0)
            {
                builder.Append(" |");
                foreach (var (key, value) in fields)
                {
                    builder.Append($" {key}={FormatValue(value)}");
                }
            }

            return builder.ToString();
        }

        private static string FormatValue(object value)
        {
            return value switch
            {
                null => "null",
                string str => $"\"{str}\"",
                DateTime dt => dt.ToString("O"),
                IFormattable f => f.ToString(null, CultureInfo.InvariantCulture),
                _ => value.ToString()
            };
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

        private static ConsoleColor GetColorForLevel(LogLevel level) => level switch
        {
            LogLevel.Error => ConsoleColor.Red,
            LogLevel.Warn => ConsoleColor.Yellow,
            LogLevel.Info => ConsoleColor.White,
            LogLevel.Debug => ConsoleColor.Gray,
            LogLevel.Trace => ConsoleColor.DarkGray,
            _ => ConsoleColor.White
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

        public void Dispose()
        {
            if (_isDisposed)
                return;

            _isDisposed = true;
            GC.SuppressFinalize(this);
        }
    }

    // Example usage
    class Program
    {
        static async Task Main()
        {
            try
            {
                await RunLoggerDemo();
            }
            catch (Exception ex)
            {
                WriteError(ex);
            }
        }

        private static async Task RunLoggerDemo()
        {
            using var logger = DatadogLogger.Instance;
            logger.Initialize(LogLevel.Debug);

            await RunTestScenario("Testing structured logging:", () =>
            {
                logger.LogInfo("User logged in", 
                    ("user_id", 123),
                    ("username", "john_doe"),
                    ("login_time", DateTime.UtcNow));

                logger.LogWarning("High CPU usage detected",
                    ("cpu_percent", 85.5),
                    ("process_id", 1234),
                    ("thread_count", 32));

                try
                {
                    throw new InvalidOperationException("Something went wrong");
                }
                catch (Exception ex)
                {
                    logger.LogError("Operation failed", ex,
                        ("operation", "data_processing"),
                        ("attempt", 3));
                }

                return Task.CompletedTask;
            });

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
            Console.WriteLine($"\n{description}");
            await action();
        }

        private static void WriteError(Exception ex)
        {
            var previousColor = Console.ForegroundColor;
            try
            {
                Console.ForegroundColor = ConsoleColor.Red;
                Console.WriteLine($"Error: {ex.Message}");
            }
            finally
            {
                Console.ForegroundColor = previousColor;
            }
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
