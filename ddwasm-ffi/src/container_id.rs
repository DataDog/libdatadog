use std::io::Cursor;

use ddcommon::container_id::CGROUP_PATH;
use js_sys::Uint8Array;
use wasm_bindgen::{prelude::wasm_bindgen, JsValue};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Deno, js_name = readFile, catch)]
    async fn read_file(path: &str) -> Result<JsValue, JsValue>;
}

#[wasm_bindgen]
pub async fn read_container_id() -> Result<JsValue, JsValue> {
    let res = read_file(CGROUP_PATH).await?;
    let contents: Uint8Array = res
        .try_into()
        .map_err(|_| JsValue::from("Cannot convert file to Uint8Array"))?;

    let cursor = Cursor::new(contents.to_vec());
    let id = ddcommon::container_id::extract_container_id_from_reader(cursor)
        .map_err(|e| e.to_string())?;
    Ok(id.into())
}
