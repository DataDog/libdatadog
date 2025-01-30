
#[inline(never)]
pub fn intermediate_function() {
    println!("Inside intermediate function");
    super::unwind_stack(); // Call the unwinding function from unwind_c.rs
    unsafe {
        *(std::ptr::null_mut() as *mut i32) = 0; // Trigger a crash
    }
    println!("Exit intermediate function");
}
