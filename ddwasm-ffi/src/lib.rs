use sketches_ddsketch::DDSketch;
use wasm_bindgen::{prelude::wasm_bindgen, JsValue};
pub mod container_id;

#[wasm_bindgen]
#[repr(transparent)]
pub struct Sketch(DDSketch);

#[wasm_bindgen]
impl Sketch {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Sketch {
        let cfg = sketches_ddsketch::Config::defaults();

        Sketch(DDSketch::new(cfg))
    }
    #[wasm_bindgen]
    pub fn add(&mut self, v: f64) {
        self.0.add(v)
    }
}

impl Default for Sketch {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Deno, js_name = readFile, catch)]
    async fn read_file(path: &str) -> Result<JsValue, JsValue>;
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(path: &str);
}
