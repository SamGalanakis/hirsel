import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { once } from 'node:events'
import { createServer } from 'node:http'
import { createRequire } from 'node:module'
import { join } from 'node:path'

const require = createRequire(join(process.env.MCP_SDK_DIR ?? '/tmp/ref-mcp-sdk', 'package.json'))
const { McpServer, createMcpHandler, fromJsonSchema } = require('@modelcontextprotocol/server')
const { toNodeHandler } = require('@modelcontextprotocol/node')
const [runner, ...args] = process.argv.slice(2)
const cancelled = Promise.withResolvers()
const handler = createMcpHandler(() => {
  const server = new McpServer({ name: 'sdk-server', version: '1' })
  server.registerTool('echo', {
    inputSchema: fromJsonSchema({ type: 'object', properties: { text: { type: 'string', 'x-mcp-header': 'Text' } }, required: ['text'] })
  }, ({ text }) => ({ content: [{ type: 'text', text }] }))
  server.registerTool('failure', {}, () => ({ isError: true, content: [{ type: 'text', text: 'tool failed' }] }))
  server.registerTool('slow', {}, async ctx => {
    await ctx.mcpReq.notify({
      method: 'notifications/progress',
      params: { progressToken: 'cancel-1', progress: 1 }
    })
    await new Promise(resolve => {
      if (ctx.mcpReq.signal.aborted) resolve()
      else ctx.mcpReq.signal.addEventListener('abort', resolve, { once: true })
    })
    cancelled.resolve()
    return { content: [] }
  })
  server.registerResource('Readme', 'fixture://notes/readme', { mimeType: 'text/plain' },
    uri => ({ contents: [{ uri: uri.href, mimeType: 'text/plain', text: 'fixture note' }] }))
  return server
}, { legacy: 'reject' })

const http = createServer(toNodeHandler(handler))
http.listen(0, '127.0.0.1')
await once(http, 'listening')
try {
  const child = spawn(runner, [...args, String(http.address().port)], { stdio: 'inherit' })
  const [status] = await once(child, 'exit')
  assert.equal(status, 0)
  const signal = AbortSignal.timeout(5000)
  const deadline = new Promise((_, reject) => signal.addEventListener('abort', () => reject(signal.reason)))
  await Promise.race([cancelled.promise, deadline])
  console.log('Hirsel client to SDK server passed')
} finally {
  await handler.close()
  http.closeAllConnections()
  http.close()
}
