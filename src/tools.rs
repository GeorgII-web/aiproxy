//! Function-calling emulation for plain chat models (`tools = "emulate"` on an agent): the client's
//! tools are described in the system prompt, and a tool call in the answer is parsed back into an
//! OpenAI tool call. The client runs the tool and sends the result as a tool message.

use serde_json::Value;

use crate::openai::{ToolCall, ToolSpec};

/// System prompt block describing the tools and the call format.
pub fn prompt(tools: &[ToolSpec]) -> String {
    let list = serde_json::to_string_pretty(tools).unwrap_or_default();
    format!(
        "# Tools\n\n\
You can use the tools below. You do not run them yourself: the application you talk to runs a tool \
when you ask for it and sends the result back in a [tool] message; then you continue.\n\n\
Tools (JSON):\n{list}\n\n\
To use a tool, end your answer with the call as a JSON object in a code block, with nothing after it:\n\n\
```tool_call\n{{\"name\": \"<tool name>\", \"arguments\": {{<arguments matching the tool's parameters>}}}}\n```\n\n\
Several calls at once: one JSON object per line in the same block. Never invent a tool result and never \
pretend a command ran: stop after the block and wait for the [tool] message. When no tool is needed, \
answer normally."
    )
}

/// Splits an answer into the text before the first tool call and the calls. `None` without a call.
/// Understands the JSON block from [`prompt`] (also without the code fence: the DeepSeek page renders
/// fences away) and DeepSeek's own `<|DSML|invoke name="..">` markup.
pub fn parse(text: &str) -> Option<(String, Vec<ToolCall>)> {
    let (start, calls) = parse_dsml(text).or_else(|| parse_json(text))?;
    if calls.is_empty() {
        return None;
    }
    let mut lead = text[..start].trim_end().to_string();
    // The lines opening the block belong to it: a fence, a `<...calls>` tag, or the code-block
    // banner of a chat page ("tool_call", "Copy", "Download").
    loop {
        let i = lead.rfind('\n').map(|i| i + 1).unwrap_or(0);
        let last = lead[i..].trim();
        let opener = last.starts_with("```")
            || (last.starts_with('<') && last.ends_with("calls>"))
            || matches!(last, "tool_call" | "json" | "Copy" | "Download");
        if last.is_empty() || !opener {
            break;
        }
        lead.truncate(i);
        lead = lead.trim_end().to_string();
    }
    Some((lead, calls))
}

fn new_id() -> String {
    crate::openai::id("call")
}

/// `invoke name="X"` ... `parameter name="Y" [string="true"]>value</...parameter>` ... `invoke>`.
fn parse_dsml(text: &str) -> Option<(usize, Vec<ToolCall>)> {
    const INVOKE: &str = "invoke name=\"";
    const PARAM: &str = "parameter name=\"";
    let first = text.find(INVOKE)?;
    let start = text[..first].rfind('<').unwrap_or(first);
    let mut calls = Vec::new();
    let mut pos = first;
    while let Some(i) = text[pos..].find(INVOKE) {
        let name_start = pos + i + INVOKE.len();
        let Some(name_len) = text[name_start..].find('"') else {
            break;
        };
        let name = text[name_start..name_start + name_len].to_string();
        let body_start = name_start + name_len;
        let body_end = text[body_start..]
            .find("invoke>")
            .map(|j| body_start + j)
            .unwrap_or(text.len());
        let body = &text[body_start..body_end];
        let mut args = serde_json::Map::new();
        let mut p = 0;
        while let Some(j) = body[p..].find(PARAM) {
            let k = p + j + PARAM.len();
            let Some(klen) = body[k..].find('"') else {
                break;
            };
            let key = body[k..k + klen].to_string();
            let Some(gt) = body[k + klen..].find('>') else {
                break;
            };
            let attrs = &body[k + klen..k + klen + gt];
            let v0 = k + klen + gt + 1;
            let vlen = body[v0..].find("</").unwrap_or(body.len() - v0);
            let raw = body[v0..v0 + vlen].trim();
            let value = if attrs.contains("string=\"true\"") {
                Value::String(raw.to_string())
            } else {
                serde_json::from_str(raw).unwrap_or(Value::String(raw.to_string()))
            };
            args.insert(key, value);
            p = v0 + vlen;
        }
        calls.push(ToolCall {
            id: new_id(),
            name,
            arguments: Value::Object(args).to_string(),
        });
        pos = body_end;
    }
    Some((start, calls))
}

/// Every top-level JSON object with a `name` and `arguments` becomes a call.
fn parse_json(text: &str) -> Option<(usize, Vec<ToolCall>)> {
    let mut calls = Vec::new();
    let mut start = None;
    let mut pos = 0;
    while let Some(i) = text[pos..].find('{') {
        let open = pos + i;
        match balanced(&text[open..])
            .and_then(|len| call_from(&text[open..open + len]).map(|c| (len, c)))
        {
            Some((len, call)) => {
                start.get_or_insert(open);
                calls.push(call);
                pos = open + len;
            }
            None => pos = open + 1,
        }
    }
    start.map(|s| (s, calls))
}

/// Length of the balanced `{...}` at the start of `s`, string-aware.
fn balanced(s: &str) -> Option<usize> {
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for (i, c) in s.char_indices() {
        match (in_str, c) {
            (true, '\\') if !esc => esc = true,
            (true, '"') if !esc => in_str = false,
            (true, _) => esc = false,
            (false, '"') => in_str = true,
            (false, '{') => depth += 1,
            (false, '}') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + c.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

fn call_from(json: &str) -> Option<ToolCall> {
    let v: Value = serde_json::from_str(json).ok()?;
    let name = v.get("name")?.as_str()?.to_string();
    let arguments = match v.get("arguments")? {
        Value::String(s) if serde_json::from_str::<Value>(s).is_ok() => s.clone(),
        Value::String(s) => Value::String(s.clone()).to_string(),
        other => other.to_string(),
    };
    Some(ToolCall {
        id: new_id(),
        name,
        arguments,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_block() {
        let text = "I will list the files.\n```tool_call\n{\"name\": \"developer__shell\", \"arguments\": {\"command\": \"ls -la\"}}\n```";
        let (lead, calls) = parse(text).unwrap();
        assert_eq!(lead, "I will list the files.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "developer__shell");
        assert_eq!(calls[0].arguments, "{\"command\":\"ls -la\"}");
    }

    #[test]
    fn json_without_fence_two_calls() {
        let text =
            "{\"name\": \"a\", \"arguments\": {}}\n{\"name\": \"b\", \"arguments\": {\"x\": 1}}";
        let (lead, calls) = parse(text).unwrap();
        assert_eq!(lead, "");
        assert_eq!(
            calls.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(calls[1].arguments, "{\"x\":1}");
    }

    #[test]
    fn dsml() {
        let text = "Let me check.\n<｜DSML｜calls>\n<｜DSML｜invoke name=\"developer__shell\">\n<｜DSML｜parameter name=\"command\" string=\"true\">cd /home && ls -la</｜DSML｜parameter>\n<｜DSML｜parameter name=\"n\">3</｜DSML｜parameter>\n</｜DSML｜invoke>\n</｜DSML｜calls>";
        let (lead, calls) = parse(text).unwrap();
        assert_eq!(lead, "Let me check.");
        assert_eq!(calls.len(), 1);
        assert_eq!(
            calls[0].arguments,
            "{\"command\":\"cd /home && ls -la\",\"n\":3}"
        );
    }

    #[test]
    fn plain_text_and_other_json() {
        assert!(parse("Just an answer with {\"a\": 1} inside.").is_none());
        assert!(parse("no braces").is_none());
    }
}
