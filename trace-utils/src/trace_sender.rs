use curl::easy::{Easy, List};
use std::{
    env,
    io::{Cursor, Read},
    str,
};

fn construct_headers() -> std::io::Result<List> {
    let api_key = match env::var("DD_API_KEY") {
        Ok(key) => key,
        Err(_) => panic!("oopsy, no DD_API_KEY was provided"),
    };
    let mut list = List::new();
    list.append(format!("User-agent: {}", "ffi-test").as_str())?;
    list.append(format!("Content-type: {}", "application/x-protobuf").as_str())?;
    list.append(format!("DD-API-KEY: {}", &api_key).as_str())?;
    list.append(format!("X-Datadog-Reported-Languages: {}", "nodejs").as_str())?;
    Ok(list)
}

pub fn send(data: Vec<u8>) -> std::io::Result<Vec<u8>> {
    let mut easy = Easy::new();
    let mut dst = Vec::new();
    let len = data.len();
    let mut data_cursor = Cursor::new(data);
    {
        easy.url("https://trace.agent.datadoghq.com/api/v0.2/traces")?;
        easy.post(true)?;
        easy.post_field_size(len as u64)?;
        easy.http_headers(construct_headers()?)?;

        let mut transfer = easy.transfer();

        transfer.read_function(|buf| Ok(data_cursor.read(buf).unwrap_or(0)))?;

        println!("PERFORMING SEND NOW");

        transfer.write_function(|result_data| {
            dst.extend_from_slice(result_data);
            match str::from_utf8(result_data) {
                Ok(v) => {
                    println!("sent-----------------");
                    println!("successfully sent:::::: {:?}", v);
                }
                Err(e) => panic!("Invalid UTF-8 sequence: {}", e),
            };
            Ok(result_data.len())
        })?;

        transfer.perform()?;
    }
    Ok(dst)
}
