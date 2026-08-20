import type { LogEntry } from '@/api/types';

export interface ChatToolCall {
  id?: string | undefined;
  name: string;
  arguments: string;
}

export interface ChatMessageItem {
  role: string;
  name?: string | undefined;
  content?: string | undefined;
  reasoning?: string | undefined;
  toolCalls?: ChatToolCall[] | undefined;
  toolUseId?: string | undefined;
}

export interface ChatToolDeclaration {
  name: string;
  description?: string | undefined;
  parameters?: Record<string, unknown> | undefined;
}

export interface ChatInspection {
  isChat: boolean;
  systemPrompt?: string | undefined;
  messages: ChatMessageItem[];
  tools: ChatToolDeclaration[];
  params: {
    temperature?: number | undefined;
    max_tokens?: number | undefined;
    top_p?: number | undefined;
    stream?: boolean | undefined;
  };
  response?:
    | {
        role: string;
        content?: string | undefined;
        reasoning?: string | undefined;
        toolCalls?: ChatToolCall[] | undefined;
        finishReason?: string | undefined;
        isStream?: boolean | undefined;
      }
    | undefined;
}

function extractTextContent(content: unknown): string {
  if (typeof content === 'string') {
    return content;
  }
  if (Array.isArray(content)) {
    const texts: string[] = [];
    for (const part of content) {
      if (typeof part === 'string') {
        texts.push(part);
      } else if (part !== null && typeof part === 'object') {
        const p = part as Record<string, unknown>;
        if (typeof p.text === 'string') {
          texts.push(p.text);
        } else if (typeof p.content === 'string') {
          texts.push(p.content);
        } else if (p.type === 'image_url' || p.type === 'image') {
          texts.push('[Image]');
        }
      }
    }
    return texts.join('\n');
  }
  if (content === null || content === undefined) {
    return '';
  }
  if (typeof content === 'number' || typeof content === 'boolean') {
    return String(content);
  }
  try {
    return JSON.stringify(content);
  } catch {
    return '';
  }
}

function parseStreamSse(sseText: string) {
  let content = '';
  let reasoning = '';
  const toolCallsMap = new Map<
    number | string,
    { id?: string | undefined; name: string; arguments: string }
  >();
  let finishReason: string | undefined;

  const lines = sseText.split('\n');
  for (const line of lines) {
    const trimmed = line.trim();
    if (!trimmed.startsWith('data:')) {
      continue;
    }
    const dataStr = trimmed.slice(5).trim();
    if (!dataStr || dataStr === '[DONE]') {
      continue;
    }
    try {
      const parsed = JSON.parse(dataStr) as Record<string, unknown>;
      if (Array.isArray(parsed.choices) && parsed.choices.length > 0) {
        const choice = parsed.choices[0] as Record<string, unknown>;
        if (typeof choice.finish_reason === 'string') {
          finishReason = choice.finish_reason;
        }
        if (choice.delta !== null && typeof choice.delta === 'object') {
          const delta = choice.delta as Record<string, unknown>;
          if (typeof delta.content === 'string') {
            content += delta.content;
          }
          if (typeof delta.reasoning_content === 'string') {
            reasoning += delta.reasoning_content;
          } else if (typeof delta.reasoning === 'string') {
            reasoning += delta.reasoning;
          }
          if (Array.isArray(delta.tool_calls)) {
            for (const tcItem of delta.tool_calls) {
              if (tcItem !== null && typeof tcItem === 'object') {
                const tc = tcItem as Record<string, unknown>;
                const index = typeof tc.index === 'number' ? tc.index : 0;
                const existing = toolCallsMap.get(index) ?? { name: '', arguments: '' };
                if (typeof tc.id === 'string') existing.id = tc.id;
                if (tc.function !== null && typeof tc.function === 'object') {
                  const fn = tc.function as Record<string, unknown>;
                  if (typeof fn.name === 'string') existing.name += fn.name;
                  if (typeof fn.arguments === 'string') existing.arguments += fn.arguments;
                }
                toolCallsMap.set(index, existing);
              }
            }
          }
        }
      }
      if (
        parsed.type === 'content_block_delta' &&
        parsed.delta !== null &&
        typeof parsed.delta === 'object'
      ) {
        const delta = parsed.delta as Record<string, unknown>;
        if (delta.type === 'text_delta' && typeof delta.text === 'string') {
          content += delta.text;
        } else if (delta.type === 'thinking_delta' && typeof delta.thinking === 'string') {
          reasoning += delta.thinking;
        } else if (delta.type === 'input_json_delta' && typeof delta.partial_json === 'string') {
          const index = typeof parsed.index === 'number' ? parsed.index : 0;
          const existing = toolCallsMap.get(index) ?? { name: '', arguments: '' };
          existing.arguments += delta.partial_json;
          toolCallsMap.set(index, existing);
        }
      }
      if (
        parsed.type === 'message_delta' &&
        parsed.delta !== null &&
        typeof parsed.delta === 'object'
      ) {
        const delta = parsed.delta as Record<string, unknown>;
        if (typeof delta.stop_reason === 'string') {
          finishReason = delta.stop_reason;
        }
      }
    } catch {
      // 单行 SSE 损坏时跳过，继续拼完整流。
    }
  }

  const toolCalls: ChatToolCall[] = [];
  for (const tc of toolCallsMap.values()) {
    if (tc.name || tc.arguments) {
      toolCalls.push(tc);
    }
  }

  return {
    content,
    reasoning,
    toolCalls,
    finishReason,
    isStream: true,
  };
}

export function parseChatInspection(
  requestText: string | null,
  responseText: string | null,
): ChatInspection {
  const result: ChatInspection = {
    isChat: false,
    messages: [],
    tools: [],
    params: {},
  };

  if (!requestText) {
    return result;
  }

  try {
    const reqObj = JSON.parse(requestText) as Record<string, unknown>;
    if (typeof reqObj.temperature === 'number') result.params.temperature = reqObj.temperature;
    if (typeof reqObj.max_tokens === 'number') result.params.max_tokens = reqObj.max_tokens;
    if (typeof reqObj.top_p === 'number') result.params.top_p = reqObj.top_p;
    if (typeof reqObj.stream === 'boolean') result.params.stream = reqObj.stream;

    if (Array.isArray(reqObj.tools)) {
      for (const item of reqObj.tools) {
        if (item !== null && typeof item === 'object') {
          const t = item as Record<string, unknown>;
          if (t.type === 'function' && t.function !== null && typeof t.function === 'object') {
            const fn = t.function as Record<string, unknown>;
            const fnName = typeof fn.name === 'string' ? fn.name : '';
            result.tools.push({
              name: fnName,
              description: typeof fn.description === 'string' ? fn.description : undefined,
              parameters:
                fn.parameters !== null && typeof fn.parameters === 'object'
                  ? (fn.parameters as Record<string, unknown>)
                  : undefined,
            });
          } else if (typeof t.name === 'string') {
            result.tools.push({
              name: t.name,
              description: typeof t.description === 'string' ? t.description : undefined,
              parameters:
                t.input_schema !== null && typeof t.input_schema === 'object'
                  ? (t.input_schema as Record<string, unknown>)
                  : undefined,
            });
          }
        }
      }
    }

    if (typeof reqObj.system === 'string') {
      result.systemPrompt = reqObj.system;
    } else if (Array.isArray(reqObj.system)) {
      result.systemPrompt = extractTextContent(reqObj.system);
    } else if (typeof reqObj.instructions === 'string') {
      result.systemPrompt = reqObj.instructions;
    }

    if (Array.isArray(reqObj.messages)) {
      result.isChat = true;
      for (const m of reqObj.messages) {
        if (m === null || typeof m !== 'object') continue;
        const msg = m as Record<string, unknown>;
        const role = typeof msg.role === 'string' ? msg.role : 'user';
        const content = extractTextContent(msg.content);
        const name = typeof msg.name === 'string' ? msg.name : undefined;
        const toolUseId = typeof msg.tool_call_id === 'string' ? msg.tool_call_id : undefined;

        const toolCalls: ChatToolCall[] = [];
        if (Array.isArray(msg.tool_calls)) {
          for (const tcItem of msg.tool_calls) {
            if (tcItem !== null && typeof tcItem === 'object') {
              const tc = tcItem as Record<string, unknown>;
              const fn =
                tc.function !== null && typeof tc.function === 'object'
                  ? (tc.function as Record<string, unknown>)
                  : {};
              const fnName = typeof fn.name === 'string' ? fn.name : '';
              toolCalls.push({
                id: typeof tc.id === 'string' ? tc.id : undefined,
                name: fnName,
                arguments:
                  typeof fn.arguments === 'string' ? fn.arguments : JSON.stringify(fn.arguments),
              });
            }
          }
        }

        if (Array.isArray(msg.content)) {
          for (const blockItem of msg.content) {
            if (blockItem !== null && typeof blockItem === 'object') {
              const block = blockItem as Record<string, unknown>;
              if (block.type === 'tool_use') {
                const blockName = typeof block.name === 'string' ? block.name : '';
                toolCalls.push({
                  id: typeof block.id === 'string' ? block.id : undefined,
                  name: blockName,
                  arguments:
                    typeof block.input === 'string' ? block.input : JSON.stringify(block.input),
                });
              }
            }
          }
        }

        result.messages.push({
          role,
          name,
          content,
          toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
          toolUseId,
        });
      }
    }
  } catch {
    // 非 JSON 请求体无法拆成对话结构。
  }

  if (responseText) {
    if (responseText.includes('data:')) {
      const streamRes = parseStreamSse(responseText);
      if (streamRes.content || streamRes.reasoning || streamRes.toolCalls.length > 0) {
        result.response = {
          role: 'assistant',
          content: streamRes.content || undefined,
          reasoning: streamRes.reasoning || undefined,
          toolCalls: streamRes.toolCalls.length > 0 ? streamRes.toolCalls : undefined,
          finishReason: streamRes.finishReason,
          isStream: true,
        };
      }
    } else {
      try {
        const resObj = JSON.parse(responseText) as Record<string, unknown>;
        if (Array.isArray(resObj.choices) && resObj.choices.length > 0) {
          const choice = resObj.choices[0] as Record<string, unknown>;
          const msg =
            choice.message !== null && typeof choice.message === 'object'
              ? (choice.message as Record<string, unknown>)
              : {};
          const toolCalls: ChatToolCall[] = [];
          if (Array.isArray(msg.tool_calls)) {
            for (const tcItem of msg.tool_calls) {
              if (tcItem !== null && typeof tcItem === 'object') {
                const tc = tcItem as Record<string, unknown>;
                const fn =
                  tc.function !== null && typeof tc.function === 'object'
                    ? (tc.function as Record<string, unknown>)
                    : {};
                const fnName = typeof fn.name === 'string' ? fn.name : '';
                toolCalls.push({
                  id: typeof tc.id === 'string' ? tc.id : undefined,
                  name: fnName,
                  arguments:
                    typeof fn.arguments === 'string' ? fn.arguments : JSON.stringify(fn.arguments),
                });
              }
            }
          }

          const msgRole = typeof msg.role === 'string' ? msg.role : 'assistant';
          result.response = {
            role: msgRole,
            content: extractTextContent(msg.content) || undefined,
            reasoning:
              typeof msg.reasoning_content === 'string'
                ? msg.reasoning_content
                : typeof msg.reasoning === 'string'
                  ? msg.reasoning
                  : undefined,
            toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
            finishReason:
              typeof choice.finish_reason === 'string' ? choice.finish_reason : undefined,
            isStream: false,
          };
        }
        if (Array.isArray(resObj.content)) {
          let content = '';
          let reasoning = '';
          const toolCalls: ChatToolCall[] = [];

          for (const blockItem of resObj.content) {
            if (blockItem === null || typeof blockItem !== 'object') continue;
            const block = blockItem as Record<string, unknown>;
            if (block.type === 'text' && typeof block.text === 'string') {
              content += (content ? '\n' : '') + block.text;
            } else if (block.type === 'thinking' && typeof block.thinking === 'string') {
              reasoning += (reasoning ? '\n' : '') + block.thinking;
            } else if (block.type === 'tool_use') {
              const blockName = typeof block.name === 'string' ? block.name : '';
              toolCalls.push({
                id: typeof block.id === 'string' ? block.id : undefined,
                name: blockName,
                arguments:
                  typeof block.input === 'string' ? block.input : JSON.stringify(block.input),
              });
            }
          }

          const resRole = typeof resObj.role === 'string' ? resObj.role : 'assistant';
          result.response = {
            role: resRole,
            content: content || undefined,
            reasoning: reasoning || undefined,
            toolCalls: toolCalls.length > 0 ? toolCalls : undefined,
            finishReason: typeof resObj.stop_reason === 'string' ? resObj.stop_reason : undefined,
            isStream: false,
          };
        }
      } catch {
        // 非 JSON 响应体无法拆成对话结构。
      }
    }
  }

  return result;
}

export function formatJsonArgs(raw: string): string {
  try {
    const parsed = JSON.parse(raw) as unknown;
    return JSON.stringify(parsed, null, 2);
  } catch {
    return raw;
  }
}

export function generateCurlCommand(entry: LogEntry, rawRequestJson: string | null): string {
  const protocol = entry.inbound_protocol;
  let endpoint = '/v1/chat/completions';
  if (protocol === 'anthropic_messages') {
    endpoint = '/v1/messages';
  } else if (protocol === 'openai_responses') {
    endpoint = '/v1/responses';
  }

  const origin = typeof window !== 'undefined' ? window.location.origin : 'http://localhost:8080';
  const url = `${origin}${endpoint}`;
  const authHeader = `Authorization: Bearer ${entry.token_key || '<TOKEN_KEY>'}`;

  let bodyStr = rawRequestJson?.trim() ?? '';
  if (!bodyStr) {
    bodyStr = JSON.stringify(
      {
        model: entry.model,
        messages: [{ role: 'user', content: 'Hello!' }],
      },
      null,
      2,
    );
  }

  return `curl -X POST "${url}" \\\n  -H "${authHeader}" \\\n  -H "Content-Type: application/json" \\\n  -d '${bodyStr.replace(/'/g, "'\\''")}'`;
}
