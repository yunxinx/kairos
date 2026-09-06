import type { Protocol } from '@/api/types';
import anthropicIcon from '@lobehub/icons-static-svg/icons/anthropic.svg';
import openaiIcon from '@lobehub/icons-static-svg/icons/openai.svg';
import geminiIcon from '@lobehub/icons-static-svg/icons/gemini.svg';

/** 协议徽章着色：三协议各自独立配色，见 globals.css 的 --proto-* 变量。 */
export const PROTOCOL_BADGE_CLASS: Record<Protocol, string> = {
  openai_chat: 'badge-proto-chat',
  openai_responses: 'badge-proto-responses',
  anthropic_messages: 'badge-proto-anthropic',
  gemini: 'badge-proto-gemini',
};

/** 协议品牌 logo：Chat / Responses 共用 OpenAI，Messages 用 Anthropic。 */
export const PROTOCOL_ICON_SRC: Record<Protocol, string> = {
  openai_chat: openaiIcon,
  openai_responses: openaiIcon,
  anthropic_messages: anthropicIcon,
  gemini: geminiIcon,
};

/** 当前渠道表上的出站协议：未加载完不算未知，缺条目才标未知。 */
export type OutboundProtocolResolution =
  | { status: 'pending' }
  | { status: 'unknown' }
  | { status: 'same'; protocol: string }
  | { status: 'converted'; protocol: string };

export function resolveOutboundProtocol(
  inbound: string,
  channel: string,
  map: Map<string, string> | null,
): OutboundProtocolResolution {
  if (map === null) {
    return { status: 'pending' };
  }
  const outbound = map.get(channel);
  if (outbound === undefined) {
    return { status: 'unknown' };
  }
  if (outbound === inbound) {
    return { status: 'same', protocol: outbound };
  }
  return { status: 'converted', protocol: outbound };
}
