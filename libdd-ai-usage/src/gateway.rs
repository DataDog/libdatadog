// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The operation a gateway's upstream request path stands for.

/// The `gen_ai.operation.name` of an upstream request path (`/v1/chat/completions`,
/// `/v1/messages`, `/v1/responses`, `/v1/embeddings`, `/v1/completions`,
/// Gemini's `:generateContent`, Bedrock's `/converse`), or an operation name
/// already. A query string is ignored. `None` for anything else, which is
/// not an operation these profiles measure.
pub fn operation_name_for(route_or_path: &str) -> Option<&'static str> {
    let path = route_or_path.trim();
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = path.trim_end_matches('/');
    match path {
        "chat" => return Some("chat"),
        "text_completion" => return Some("text_completion"),
        "embeddings" => return Some("embeddings"),
        _ => {}
    }
    let lower = path.to_ascii_lowercase();
    let ends = |suffix: &str| lower.ends_with(suffix);
    if ends("/embeddings") || ends(":embedcontent") || ends(":batchembedcontents") || ends("/embed")
    {
        Some("embeddings")
    } else if ends("/chat/completions")
        || ends("/messages")
        || ends("/responses")
        || ends(":generatecontent")
        || ends(":streamgeneratecontent")
        || ends("/converse")
        || ends("/converse-stream")
    {
        Some("chat")
    } else if ends("/completions") {
        Some("text_completion")
    } else {
        None
    }
}
