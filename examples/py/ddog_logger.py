import ctypes
import threading
from ctypes import c_int, c_char_p, c_size_t, POINTER, CFUNCTYPE, Structure, c_ubyte


# === Match Rust Structs ===

class CharSlice(Structure):
    _fields_ = [
        ("data", POINTER(c_ubyte)),
        ("len", c_size_t),
    ]

    def to_str(self):
        if not self.data or self.len == 0:
            return ""
        array_type = c_ubyte * self.len
        byte_array = ctypes.cast(self.data, POINTER(array_type)).contents
        return bytearray(byte_array).decode("utf-8", errors="replace")


class LogField(Structure):
    _fields_ = [
        ("key", CharSlice),
        ("value", CharSlice),
    ]


class LogFieldVec(Structure):
    _fields_ = [
        ("ptr", POINTER(LogField)),
        ("len", c_size_t),
        ("capacity", c_size_t),
    ]

    def to_list(self):
        if not self.ptr or self.len == 0:
            return []
        return [self.ptr[i] for i in range(self.len)]


class LogEvent(Structure):
    _fields_ = [
        ("level", c_int),
        ("message", CharSlice),
        ("fields", LogFieldVec),
    ]


# === LogLevel enum ===
class LogLevel:
    DEBUG = 0
    INFO = 1
    WARN = 2
    ERROR = 3
    TRACE = 4


# === Callback type ===
CALLBACK_TYPE = CFUNCTYPE(None, LogEvent)


# === Load the compiled Rust library ===
lib = ctypes.CDLL("/Users/ganesh.jangir/dd/libdatadog/target/debug/libdata_pipeline_ffi.dylib")  # or .so/.dll as needed


# === Hold callback reference to prevent GC ===
_callback_holder = None


# === Error checker (Optional[Box<Error>] as *mut Error, cast to c_void_p) ===
def _check_error(result):
    if result:
        raise RuntimeError("Rust returned an error (Box<Error>) — need ddog_error_message for details")
    return None


# === Bind FFI functions ===

lib.ddog_logger_init.argtypes = [c_int, CALLBACK_TYPE]
lib.ddog_logger_init.restype = ctypes.c_void_p

lib.ddog_logger_set_max_log_level.argtypes = [c_int]
lib.ddog_logger_set_max_log_level.restype = ctypes.c_void_p

lib.trigger_logs.argtypes = []
lib.trigger_logs.restype = None

lib.trigger_logs_with_args.argtypes = []
lib.trigger_logs_with_args.restype = None


# === Public wrappers ===

def ddog_logger_init(level: int, callback):
    global _callback_holder
    cb = CALLBACK_TYPE(callback)
    _callback_holder = cb
    result = lib.ddog_logger_init(level, cb)
    _check_error(result)


def ddog_logger_set_max_log_level(level: int):
    result = lib.ddog_logger_set_max_log_level(level)
    _check_error(result)


def trigger_logs():
    lib.trigger_logs()


def trigger_logs_with_args():
    lib.trigger_logs_with_args()


# === Example callback ===

def log_callback(event: LogEvent):
    level_name = {
        LogLevel.DEBUG: "DEBUG",
        LogLevel.INFO: "INFO",
        LogLevel.WARN: "WARN",
        LogLevel.ERROR: "ERROR",
        LogLevel.TRACE: "TRACE",
    }.get(event.level, f"UNKNOWN({event.level})")

    thread_id = threading.current_thread()
    print(f"[{thread_id}]")

    message = event.message.to_str()
    print(f"[{level_name}] {message}")

    for field in event.fields.to_list():
        key = field.key.to_str()
        value = field.value.to_str()
        print(f"  {key} = {value}")


# === Run test ===

if __name__ == "__main__":
    ddog_logger_init(LogLevel.DEBUG, log_callback)
    # for i in range(100000):
    trigger_logs_with_args()

