//! Wire form of OpenAI Responses API SSE chunk.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiResponsesStreamChunk {
    pub r#type: OpenAiResponsesStreamEventType,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenAiResponsesStreamEventType {
    ResponseCreated,
    ResponseInProgress,
    ResponseCompleted,
    ResponseFailed,
    ResponseIncomplete,
    ResponseQueued,
    ResponseCancelled,

    ResponseOutputItemAdded,
    ResponseOutputItemDone,

    ResponseContentPartAdded,
    ResponseContentPartDone,

    ResponseTextDelta,
    ResponseTextDone,
    ResponseOutputTextDelta,
    ResponseOutputTextDone,

    ResponseRefusalDelta,
    ResponseRefusalDone,

    ResponseFunctionCallArgumentsDelta,
    ResponseFunctionCallArgumentsDone,

    ResponseReasoningSummaryPartAdded,
    ResponseReasoningSummaryPartDone,
    ResponseReasoningSummaryTextDelta,
    ResponseReasoningSummaryTextDone,
    ResponseReasoningTextDelta,
    ResponseReasoningTextDone,

    ResponseFileSearchCallInProgress,
    ResponseFileSearchCallSearching,
    ResponseFileSearchCallCompleted,

    ResponseWebSearchCallInProgress,
    ResponseWebSearchCallSearching,
    ResponseWebSearchCallCompleted,

    ResponseCodeInterpreterCallInProgress,
    ResponseCodeInterpreterCallInterpreting,
    ResponseCodeInterpreterCallCompleted,
    ResponseCodeInterpreterCallCodeDelta,
    ResponseCodeInterpreterCallCodeDone,

    ResponseImageGenerationCallInProgress,
    ResponseImageGenerationCallGenerating,
    ResponseImageGenerationCallPartialImage,
    ResponseImageGenerationCallCompleted,

    ResponseMcpCallInProgress,
    ResponseMcpCallExecuting,
    ResponseMcpCallCompleted,
    ResponseMcpListToolsInProgress,
    ResponseMcpListToolsCompleted,
    ResponseMcpListToolsFailed,

    ResponseAudioDelta,
    ResponseAudioDone,
    ResponseAudioTranscriptDelta,
    ResponseAudioTranscriptDone,

    ResponseOutputTextAnnotationAdded,

    ResponseCustomToolCallInputDelta,
    ResponseCustomToolCallInputDone,

    ResponseError,
    Unknown(String),
}

impl Serialize for OpenAiResponsesStreamEventType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl OpenAiResponsesStreamEventType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::ResponseCreated => "response.created",
            Self::ResponseInProgress => "response.in_progress",
            Self::ResponseCompleted => "response.completed",
            Self::ResponseFailed => "response.failed",
            Self::ResponseIncomplete => "response.incomplete",
            Self::ResponseQueued => "response.queued",
            Self::ResponseCancelled => "response.cancelled",
            Self::ResponseOutputItemAdded => "response.output_item.added",
            Self::ResponseOutputItemDone => "response.output_item.done",
            Self::ResponseContentPartAdded => "response.content_part.added",
            Self::ResponseContentPartDone => "response.content_part.done",
            Self::ResponseTextDelta => "response.text.delta",
            Self::ResponseTextDone => "response.text.done",
            Self::ResponseOutputTextDelta => "response.output_text.delta",
            Self::ResponseOutputTextDone => "response.output_text.done",
            Self::ResponseRefusalDelta => "response.refusal.delta",
            Self::ResponseRefusalDone => "response.refusal.done",
            Self::ResponseFunctionCallArgumentsDelta => "response.function_call_arguments.delta",
            Self::ResponseFunctionCallArgumentsDone => "response.function_call_arguments.done",
            Self::ResponseReasoningSummaryPartAdded => "response.reasoning_summary_part.added",
            Self::ResponseReasoningSummaryPartDone => "response.reasoning_summary_part.done",
            Self::ResponseReasoningSummaryTextDelta => "response.reasoning_summary_text.delta",
            Self::ResponseReasoningSummaryTextDone => "response.reasoning_summary_text.done",
            Self::ResponseReasoningTextDelta => "response.reasoning_text.delta",
            Self::ResponseReasoningTextDone => "response.reasoning_text.done",
            Self::ResponseFileSearchCallInProgress => "response.file_search_call.in_progress",
            Self::ResponseFileSearchCallSearching => "response.file_search_call.searching",
            Self::ResponseFileSearchCallCompleted => "response.file_search_call.completed",
            Self::ResponseWebSearchCallInProgress => "response.web_search_call.in_progress",
            Self::ResponseWebSearchCallSearching => "response.web_search_call.searching",
            Self::ResponseWebSearchCallCompleted => "response.web_search_call.completed",
            Self::ResponseCodeInterpreterCallInProgress => {
                "response.code_interpreter_call.in_progress"
            }
            Self::ResponseCodeInterpreterCallInterpreting => {
                "response.code_interpreter_call.interpreting"
            }
            Self::ResponseCodeInterpreterCallCompleted => {
                "response.code_interpreter_call.completed"
            }
            Self::ResponseCodeInterpreterCallCodeDelta => {
                "response.code_interpreter_call.code.delta"
            }
            Self::ResponseCodeInterpreterCallCodeDone => "response.code_interpreter_call.code.done",
            Self::ResponseImageGenerationCallInProgress => {
                "response.image_generation_call.in_progress"
            }
            Self::ResponseImageGenerationCallGenerating => {
                "response.image_generation_call.generating"
            }
            Self::ResponseImageGenerationCallPartialImage => {
                "response.image_generation_call.partial_image"
            }
            Self::ResponseImageGenerationCallCompleted => {
                "response.image_generation_call.completed"
            }
            Self::ResponseMcpCallInProgress => "response.mcp_call.in_progress",
            Self::ResponseMcpCallExecuting => "response.mcp_call.executing",
            Self::ResponseMcpCallCompleted => "response.mcp_call.completed",
            Self::ResponseMcpListToolsInProgress => "response.mcp_list_tools.in_progress",
            Self::ResponseMcpListToolsCompleted => "response.mcp_list_tools.completed",
            Self::ResponseMcpListToolsFailed => "response.mcp_list_tools.failed",
            Self::ResponseAudioDelta => "response.audio.delta",
            Self::ResponseAudioDone => "response.audio.done",
            Self::ResponseAudioTranscriptDelta => "response.audio_transcript.delta",
            Self::ResponseAudioTranscriptDone => "response.audio_transcript.done",
            Self::ResponseOutputTextAnnotationAdded => "response.output_text.annotation.added",
            Self::ResponseCustomToolCallInputDelta => "response.custom_tool_call_input.delta",
            Self::ResponseCustomToolCallInputDone => "response.custom_tool_call_input.done",
            Self::ResponseError => "response.error",
            Self::Unknown(s) => s.as_str(),
        }
    }
}

impl<'de> Deserialize<'de> for OpenAiResponsesStreamEventType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(match s.as_str() {
            "response.created" => Self::ResponseCreated,
            "response.in_progress" => Self::ResponseInProgress,
            "response.completed" => Self::ResponseCompleted,
            "response.failed" => Self::ResponseFailed,
            "response.incomplete" => Self::ResponseIncomplete,
            "response.queued" => Self::ResponseQueued,
            "response.cancelled" => Self::ResponseCancelled,
            "response.output_item.added" => Self::ResponseOutputItemAdded,
            "response.output_item.done" => Self::ResponseOutputItemDone,
            "response.content_part.added" => Self::ResponseContentPartAdded,
            "response.content_part.done" => Self::ResponseContentPartDone,
            "response.text.delta" => Self::ResponseTextDelta,
            "response.text.done" => Self::ResponseTextDone,
            "response.output_text.delta" => Self::ResponseOutputTextDelta,
            "response.output_text.done" => Self::ResponseOutputTextDone,
            "response.refusal.delta" => Self::ResponseRefusalDelta,
            "response.refusal.done" => Self::ResponseRefusalDone,
            "response.function_call_arguments.delta" => Self::ResponseFunctionCallArgumentsDelta,
            "response.function_call_arguments.done" => Self::ResponseFunctionCallArgumentsDone,
            "response.reasoning_summary_part.added" => Self::ResponseReasoningSummaryPartAdded,
            "response.reasoning_summary_part.done" => Self::ResponseReasoningSummaryPartDone,
            "response.reasoning_summary_text.delta" => Self::ResponseReasoningSummaryTextDelta,
            "response.reasoning_summary_text.done" => Self::ResponseReasoningSummaryTextDone,
            "response.reasoning_text.delta" => Self::ResponseReasoningTextDelta,
            "response.reasoning_text.done" => Self::ResponseReasoningTextDone,
            "response.file_search_call.in_progress" => Self::ResponseFileSearchCallInProgress,
            "response.file_search_call.searching" => Self::ResponseFileSearchCallSearching,
            "response.file_search_call.completed" => Self::ResponseFileSearchCallCompleted,
            "response.web_search_call.in_progress" => Self::ResponseWebSearchCallInProgress,
            "response.web_search_call.searching" => Self::ResponseWebSearchCallSearching,
            "response.web_search_call.completed" => Self::ResponseWebSearchCallCompleted,
            "response.code_interpreter_call.in_progress" => {
                Self::ResponseCodeInterpreterCallInProgress
            }
            "response.code_interpreter_call.interpreting" => {
                Self::ResponseCodeInterpreterCallInterpreting
            }
            "response.code_interpreter_call.completed" => {
                Self::ResponseCodeInterpreterCallCompleted
            }
            "response.code_interpreter_call.code.delta" => {
                Self::ResponseCodeInterpreterCallCodeDelta
            }
            "response.code_interpreter_call.code.done" => Self::ResponseCodeInterpreterCallCodeDone,
            "response.image_generation_call.in_progress" => {
                Self::ResponseImageGenerationCallInProgress
            }
            "response.image_generation_call.generating" => {
                Self::ResponseImageGenerationCallGenerating
            }
            "response.image_generation_call.partial_image" => {
                Self::ResponseImageGenerationCallPartialImage
            }
            "response.image_generation_call.completed" => {
                Self::ResponseImageGenerationCallCompleted
            }
            "response.mcp_call.in_progress" => Self::ResponseMcpCallInProgress,
            "response.mcp_call.executing" => Self::ResponseMcpCallExecuting,
            "response.mcp_call.completed" => Self::ResponseMcpCallCompleted,
            "response.mcp_list_tools.in_progress" => Self::ResponseMcpListToolsInProgress,
            "response.mcp_list_tools.completed" => Self::ResponseMcpListToolsCompleted,
            "response.mcp_list_tools.failed" => Self::ResponseMcpListToolsFailed,
            "response.audio.delta" => Self::ResponseAudioDelta,
            "response.audio.done" => Self::ResponseAudioDone,
            "response.audio_transcript.delta" => Self::ResponseAudioTranscriptDelta,
            "response.audio_transcript.done" => Self::ResponseAudioTranscriptDone,
            "response.output_text.annotation.added" => Self::ResponseOutputTextAnnotationAdded,
            "response.custom_tool_call_input.delta" => Self::ResponseCustomToolCallInputDelta,
            "response.custom_tool_call_input.done" => Self::ResponseCustomToolCallInputDone,
            "response.error" => Self::ResponseError,
            _ => Self::Unknown(s),
        })
    }
}
