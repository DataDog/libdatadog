// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! The operation of an upstream request path.

use libdd_ai_usage::operation_name_for;

#[test]
fn operations_from_routes_and_paths() {
    for (route, operation) in [
        // A gateway's own route names are the gateway's to map.
        ("llm/v1/chat", None),
        ("chat", Some("chat")),
        ("/v1/chat/completions?api-version=2024-10-21", Some("chat")),
        ("/v1/messages", Some("chat")),
        ("/openai/v1/responses/", Some("chat")),
        ("/v1/embeddings", Some("embeddings")),
        ("/v1/completions", Some("text_completion")),
        (
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
            Some("chat"),
        ),
        (
            "/v1/models/text-embedding-004:embedContent",
            Some("embeddings"),
        ),
        (
            "/model/anthropic.claude-sonnet-4/converse-stream",
            Some("chat"),
        ),
        ("/v1/models", None),
        ("preserve", None),
        ("", None),
    ] {
        assert_eq!(operation_name_for(route), operation, "{route}");
    }
}
