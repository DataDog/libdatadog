using System;
using System.Runtime.InteropServices;
using System.Text;

class Program
{
    // Define the LogLevel enum to match Rust
    public enum LogLevel
    {
        Debug = 0,
        Info = 1,
        Warn = 2,
        Error = 3,
        Trace = 4
    }

    // CharSlice struct to match Rust's CharSlice
    [StructLayout(LayoutKind.Sequential)]
    internal struct CharSlice
    {
        internal IntPtr Ptr;     // Matches *const T
        internal nuint Len;      // Matches usize
    }

    [StructLayout(LayoutKind.Sequential)]
    internal struct LoggerError
    {
        internal CharSlice Message;
    }

    private delegate void LogCallback(LogLevel level, CharSlice message);

    const string dllPath = "../../../../../..//target/debug/libdata_pipeline_ffi.dylib";

    [DllImport(dllPath)]
    private static extern IntPtr ddog_ffi_logger_set_log_callback(LogCallback callback);

    [DllImport(dllPath)]
    private static extern void trigger_logs_with_message(LogLevel level, CharSlice message);

    [DllImport(dllPath)]
    private static extern void ddog_ffi_logger_set_max_log_level(LogLevel level);

    [DllImport(dllPath)]
    private static extern void ddog_Error_drop(IntPtr ddog_handle);


    // Updated log handler to work with CharSlice
    private static void LogHandler(LogLevel level, CharSlice slice)
    {
        unsafe
        {
            if (slice.Ptr == IntPtr.Zero)
            {
                Console.WriteLine("⚠️ Received null message pointer!");
                return;
            }

            // Use the actual length to decode the string
            var message = Encoding.UTF8.GetString((byte*)slice.Ptr, (int)slice.Len);

            // Print log message with enum-based level
            switch (level)
            {
                case LogLevel.Error:
                    Console.ForegroundColor = ConsoleColor.Red;
                    Console.Write("[ERROR] ");
                    break;
                case LogLevel.Warn:
                    Console.ForegroundColor = ConsoleColor.Yellow;
                    Console.Write("[WARN] ");
                    break;
                case LogLevel.Info:
                    Console.ForegroundColor = ConsoleColor.White;
                    Console.Write("[INFO] ");
                    break;
                case LogLevel.Debug:
                    Console.ForegroundColor = ConsoleColor.Gray;
                    Console.Write("[DEBUG] ");
                    break;
                case LogLevel.Trace:
                    Console.ForegroundColor = ConsoleColor.DarkGray;
                    Console.Write("[TRACE] ");
                    break;
            }

            Console.WriteLine(message);
            Console.ResetColor();

        }
    }

    // Helper method to create CharSlice from string
    private static unsafe CharSlice CreateCharSlice(string message)
    {
        byte[] utf8Bytes = Encoding.UTF8.GetBytes(message);
        IntPtr ptr = Marshal.AllocHGlobal(utf8Bytes.Length);
        Marshal.Copy(utf8Bytes, 0, ptr, utf8Bytes.Length);

        return new CharSlice
        {
            Ptr = ptr,
            Len = (nuint)utf8Bytes.Length
        };
    }

    // Helper method to trigger log with custom message
    public static void TriggerCustomLog(LogLevel level, string message)
    {
        unsafe
        {
            var slice = CreateCharSlice(message);
            trigger_logs_with_message(level, slice);
        }
    }

    // Add a helper method for better usability
    public static void SetMaxLogLevel(LogLevel level)
    {
        ddog_ffi_logger_set_max_log_level(level);
    }

    // Helper method to handle logger initialization
    public static void InitializeLogger()
    {
        IntPtr errorPtr = ddog_ffi_logger_set_log_callback(LogHandler);
        if (errorPtr != IntPtr.Zero)
        {
            try
            {
                var error = Marshal.PtrToStructure<LoggerError>(errorPtr);
                var message = GetStringFromCharSlice(error.Message);
                Console.WriteLine($"Failed to initialize logger: {message}");
            }
            finally
            {
                ddog_Error_drop(errorPtr);
            }
            throw new Exception("Failed to initialize logger");
        }
        Console.WriteLine("Logger initialized successfully");

        // Set initial log level to Debug to see most logs
        SetMaxLogLevel(LogLevel.Debug);
    }

    // Helper method to convert CharSlice to string
    private static unsafe string GetStringFromCharSlice(CharSlice slice)
    {
        if (slice.Ptr == IntPtr.Zero)
        {
            return string.Empty;
        }

        byte* ptr = (byte*)slice.Ptr;
        int len = (int)slice.Len;
        return Encoding.UTF8.GetString(ptr, len);
    }

    static void Main()
    {
        try
        {
            InitializeLogger();  // This will now set log level to Debug by default
            Console.WriteLine("Logger initialized with Debug level");

            // Example usage of custom logs
            TriggerCustomLog(LogLevel.Error, "🔥 Custom error message!");   // Will show
            TriggerCustomLog(LogLevel.Warn, "⚠️ Custom warning message!");  // Will show
            TriggerCustomLog(LogLevel.Info, "ℹ️ Custom info message!");     // Will show
            TriggerCustomLog(LogLevel.Debug, "🐛 Custom debug message!");   // Will show
            TriggerCustomLog(LogLevel.Trace, "🔍 Custom trace message!");   // Won't show
        }
        catch (Exception ex)
        {
            Console.ForegroundColor = ConsoleColor.Red;
            Console.WriteLine($"Error: {ex.Message}");
            Console.ResetColor();
        }
    }
}
