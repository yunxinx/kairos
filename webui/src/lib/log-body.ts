/** 日志 body 解码结果：JSON 可读化、纯文本或二进制安全提示。 */
export type DecodedLogBody =
  | { kind: 'empty' }
  | { kind: 'json'; text: string }
  | { kind: 'text'; text: string }
  | { kind: 'binary'; byteLength: number };

function bytesFromBase64(base64: string): Uint8Array | null {
  try {
    const binary = atob(base64);
    const bytes = new Uint8Array(binary.length);
    for (let index = 0; index < binary.length; index += 1) {
      bytes[index] = binary.charCodeAt(index);
    }
    return bytes;
  } catch {
    return null;
  }
}

/**
 * 把管理 API 返回的 base64 body 解码为可展示文本。
 * 非法 UTF-8 或含 NUL 视为二进制，不尝试当文本渲染。
 */
export function decodeLogBody(base64: string | null): DecodedLogBody {
  if (base64 === null || base64.length === 0) {
    return { kind: 'empty' };
  }
  const bytes = bytesFromBase64(base64);
  if (bytes === null) {
    return { kind: 'binary', byteLength: 0 };
  }
  let text: string;
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    return { kind: 'binary', byteLength: bytes.byteLength };
  }
  if (text.includes('\u0000')) {
    return { kind: 'binary', byteLength: bytes.byteLength };
  }
  try {
    const parsed: unknown = JSON.parse(text);
    return { kind: 'json', text: JSON.stringify(parsed, null, 2) };
  } catch {
    return { kind: 'text', text };
  }
}
