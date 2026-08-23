import { DatabaseSync } from 'node:sqlite';
import { readE2eRuntime } from './gateway';

export const MS_PER_DAY = 86_400_000;

/** 播种一条请求日志时需要变化的字段；其余列用测试缺省。 */
export interface SeedLogInput {
  created_at: number;
  token_key: string;
  model: string;
  /** 实际出站模型名；缺省为 null（等于入站）。 */
  outbound_model?: string | null;
  channel: string;
  token_name?: string;
  inbound_protocol?: string;
  status_code?: number;
  latency_ms?: number;
  input_tokens?: number;
  output_tokens?: number;
  base_cost_usd_micros?: number;
  discount_bp?: number;
  cost_usd_micros?: number;
  settled?: boolean;
  request_body?: Uint8Array | null;
  response_body?: Uint8Array | null;
}

/** UTC 日历日起点（unix 毫秒），与存储层 `div_euclid` 日切口径一致。 */
export function utcDayStart(millis: number): number {
  return Math.floor(millis / MS_PER_DAY) * MS_PER_DAY;
}

/** UTF-8 字节，供 JSON/文本 body 播种。 */
export function utf8Bytes(text: string): Uint8Array {
  return new TextEncoder().encode(text);
}

/** 把请求日志直接写入 e2e 网关 SQLite，返回插入的自增 id。 */
export function seedRequestLogs(logs: SeedLogInput[]): number[] {
  const { dbPath } = readE2eRuntime();
  const db = new DatabaseSync(dbPath);
  try {
    db.exec('PRAGMA busy_timeout = 5000');
    const stmt = db.prepare(
      `INSERT INTO request_log (
         created_at, token_name, token_key, inbound_protocol, model, outbound_model, channel,
         status_code, latency_ms, input_tokens, output_tokens, cache_read_tokens,
         cache_write_tokens, input_price_usd_micros, output_price_usd_micros,
         cache_read_price_usd_micros, cache_write_price_usd_micros,
         base_cost_usd_micros, discount_bp, cost_usd_micros,
         settled, request_body, response_body
       ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, 0, 0, 0, 0, 0, ?, ?, ?, ?, ?, ?)`,
    );
    const ids: number[] = [];
    db.exec('BEGIN');
    try {
      for (const log of logs) {
        const result = stmt.run(
          log.created_at,
          log.token_name ?? 'e2e',
          log.token_key,
          log.inbound_protocol ?? 'openai_chat',
          log.model,
          log.outbound_model ?? null,
          log.channel,
          log.status_code ?? 200,
          log.latency_ms ?? 12,
          log.input_tokens ?? 0,
          log.output_tokens ?? 0,
          log.base_cost_usd_micros ?? log.cost_usd_micros ?? 0,
          log.discount_bp ?? 10000,
          log.cost_usd_micros ?? 0,
          log.settled === false ? 0 : 1,
          log.request_body ?? null,
          log.response_body ?? null,
        );
        ids.push(Number(result.lastInsertRowid));
      }
      db.exec('COMMIT');
    } catch (error) {
      db.exec('ROLLBACK');
      throw error;
    }
    return ids;
  } finally {
    db.close();
  }
}

/** 播种一条系统日志时需要变化的字段。 */
export interface SeedSystemLogInput {
  created_at?: number;
  level?: string;
  target: string;
  message: string;
}

/** 把系统日志直接写入 e2e 网关 SQLite。 */
export function seedSystemLogs(logs: SeedSystemLogInput[]): number[] {
  const { dbPath } = readE2eRuntime();
  const db = new DatabaseSync(dbPath);
  try {
    db.exec('PRAGMA busy_timeout = 5000');
    const stmt = db.prepare(
      `INSERT INTO system_log (created_at, level, target, message) VALUES (?, ?, ?, ?)`,
    );
    const ids: number[] = [];
    const now = Date.now();
    db.exec('BEGIN');
    try {
      for (const [index, log] of logs.entries()) {
        const result = stmt.run(
          log.created_at ?? now - index,
          log.level ?? 'error',
          log.target,
          log.message,
        );
        ids.push(Number(result.lastInsertRowid));
      }
      db.exec('COMMIT');
    } catch (error) {
      db.exec('ROLLBACK');
      throw error;
    }
    return ids;
  } finally {
    db.close();
  }
}
