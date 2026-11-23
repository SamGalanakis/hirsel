import assert from 'node:assert/strict'
import { writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { join } from 'node:path'

const require = createRequire(join(process.env.MCP_SDK_DIR ?? '/tmp/ref-mcp-sdk', 'package.json'))
const { Client, StreamableHTTPClientTransport } = require('@modelcontextprotocol/client')
const [url, directory] = process.argv.slice(2)
const origin = 'http://mcp.local'
const revision = '2026-07-28'
const client = new Client({ name: 'sdk-client', version: '1' }, {
  versionNegotiation: { mode: { pin: revision } }
})
const transport = new StreamableHTTPClientTransport(new URL(url), {
  requestInit: { headers: { Origin: origin } }
})

try {
  await client.connect(transport)
  assert.equal(client.getServerVersion().name, 'hirsel-mcp')
  assert.deepEqual((await client.listTools()).tools.map(tool => tool.name), ['echo', 'progress'])
  const result = await client.callTool({ name: 'echo', arguments: { text: 'from the SDK' } })
  assert.deepEqual(result.content, [{ type: 'text', text: 'from the SDK' }])
  assert.equal((await client.callTool({ name: 'echo' })).isError, true)
  assert.equal((await client.listResources()).resources[0].uri, 'fixture://notes/readme')
  assert.equal((await client.readResource({ uri: 'fixture://notes/readme' })).contents[0].text, 'fixture note')

  const abort = new AbortController()
  let first
  await assert.rejects(client.callTool({ name: 'progress', arguments: { cancel: true } }, {
    signal: abort.signal,
    onprogress(progress) {
      first = progress
      abort.abort()
    }
  }))
  assert.equal(first.progress, 1)
  const progress = []
  const completed = await client.callTool({ name: 'progress' }, {
    onprogress(update) { progress.push(update.progress) }
  })
  assert.deepEqual(progress, [1])
  assert.equal(completed.content[0].text, 'complete')

  const changed = Promise.withResolvers()
  client.setNotificationHandler('notifications/tools/list_changed', () => changed.resolve())
  const subscription = await client.listen({ toolsListChanged: true })
  assert.deepEqual(subscription.honoredFilter, { toolsListChanged: true })
  await writeFile(join(directory, 'catalog.changed'), 'changed')
  const signal = AbortSignal.timeout(5000)
  const deadline = new Promise((_, reject) => signal.addEventListener('abort', () => reject(signal.reason)))
  await Promise.race([changed.promise, deadline])
  assert.deepEqual((await client.listTools()).tools.map(tool => tool.name), ['echo', 'progress', 'added'])
  await subscription.close()
  assert.equal(await subscription.closed, 'local')
  await client.discover()

  const metadata = {
    'io.modelcontextprotocol/protocolVersion': revision,
    'io.modelcontextprotocol/clientCapabilities': {}
  }
  const headers = {
    'Content-Type': 'application/json', Accept: 'application/json, text/event-stream',
    'MCP-Protocol-Version': revision, 'Mcp-Method': 'server/discover'
  }
  const body = JSON.stringify({ jsonrpc: '2.0', id: 'test', method: 'server/discover', params: { _meta: metadata } })
  assert.equal((await fetch(url, { method: 'POST', headers: { ...headers, Origin: 'https://other.example' }, body })).status, 403)
  const mismatch = await fetch(url, { method: 'POST', headers: { ...headers, 'Mcp-Method': 'tools/list' }, body })
  assert.equal(mismatch.status, 400)
  assert.equal((await mismatch.json()).error.code, -32020)
  assert.equal((await fetch(url)).status, 405)
  console.log('SDK client to Hirsel server passed')
} finally {
  await client.close()
}
