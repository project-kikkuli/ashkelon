use crate::config::HookEvent;

/// One firing of an event, carrying whatever context a hook's stdin payload needs.
#[derive(Clone, Debug)]
pub struct HookTrigger {
    pub event: HookEvent,
    pub prompt: Option<String>,
    pub tool_calls: Vec<String>,
    pub text: Option<String>,
}

impl HookTrigger {
    pub fn new(event: HookEvent) -> HookTrigger {
        HookTrigger { event, prompt: None, tool_calls: Vec::new(), text: None }
    }

    pub fn with_prompt(event: HookEvent, prompt: String) -> HookTrigger {
        HookTrigger { event, prompt: Some(prompt), tool_calls: Vec::new(), text: None }
    }

    pub fn with_tool_calls(event: HookEvent, tool_calls: Vec<String>) -> HookTrigger {
        HookTrigger { event, prompt: None, tool_calls, text: None }
    }

    pub fn with_text(event: HookEvent, text: String) -> HookTrigger {
        HookTrigger { event, prompt: None, tool_calls: Vec::new(), text: Some(text) }
    }
}

pub fn event_name(event: HookEvent) -> &'static str {
    match event {
        HookEvent::SessionStart => "session_start",
        HookEvent::Prompt => "prompt",
        HookEvent::ToolCall => "tool_call",
        HookEvent::ToolResult => "tool_result",
        HookEvent::TurnEnd => "turn_end",
        HookEvent::Compaction => "compaction",
    }
}
