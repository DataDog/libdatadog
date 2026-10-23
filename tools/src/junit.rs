// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Minimal JUnit XML parsing: each test case's name, duration and pass/fail status.

use quick_xml::Reader;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, Event};

pub struct TestCase {
    pub name: String,
    pub secs: f64,
    pub ok: bool,
}

pub fn parse(xml: &str) -> Vec<TestCase> {
    let mut reader = Reader::from_str(xml);
    let mut cases = Vec::new();
    let mut current: Option<(String, f64)> = None;
    let mut ok = true;

    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e)) => match e.name().as_ref() {
                b"testcase" => {
                    current = Some(testcase_attributes(e));
                    ok = true;
                }
                b"failure" | b"error" if current.is_some() => ok = false,
                _ => {}
            },
            Ok(Event::End(ref e)) if e.name().as_ref() == b"testcase" => {
                if let Some((name, secs)) = current.take() {
                    if !name.is_empty() {
                        cases.push(TestCase { name, secs, ok });
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => {}
        }
    }
    cases
}

fn testcase_attributes(e: &BytesStart) -> (String, f64) {
    let mut name = String::new();
    let mut secs = 0.0;
    for attr in e.attributes().filter_map(|a| a.ok()) {
        match attr.key.as_ref() {
            b"name" => name = attribute_value(&attr),
            b"time" => secs = attribute_value(&attr).parse().unwrap_or(0.0),
            _ => {}
        }
    }
    (name, secs)
}

fn attribute_value(attr: &Attribute) -> String {
    String::from_utf8_lossy(&attr.value).to_string()
}
