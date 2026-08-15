import http from 'node:http';

/**
 * 渠道探测用的最小上游：按给定状态码回 JSON，不解析请求体。
 * 配置 `models` 后，GET /models 返回模型列表（200），供模型清单同步用例使用。
 */
export async function startProbeUpstream(
  statusCode: number,
  options: { models?: string[] } = {},
): Promise<{
  baseUrl: string;
  close: () => Promise<void>;
}> {
  const server = http.createServer((req, res) => {
    if (req.method === 'GET' && req.url === '/models' && options.models !== undefined) {
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify({ data: options.models.map((id) => ({ id })) }));
      return;
    }
    res.writeHead(statusCode, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ error: { message: `upstream ${statusCode}` } }));
  });

  await new Promise<void>((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      resolve();
    });
  });

  const address = server.address();
  if (address === null || typeof address === 'string') {
    throw new Error('probe upstream did not bind a TCP port');
  }

  return {
    baseUrl: `http://127.0.0.1:${address.port}`,
    close: () =>
      new Promise((resolve, reject) => {
        server.close((err) => {
          if (err) {
            reject(err);
            return;
          }
          resolve();
        });
      }),
  };
}
