import { describe, expect, it } from "vitest";
import { parseThreadLink, threadPath, threadReference, threadUrl } from "./thread-url";
const history = "ab123456-1234-5678-9abc-123456789abc";
const target = {kind:"thread" as const,history_id:history,thread_id:0};
describe("portable Thread URLs",()=>{
 it("round-trips zero and ordinary IDs using the current origin and Markdown",()=>{
  expect(parseThreadLink(threadPath(target))).toEqual({kind:"thread",target});
  expect(parseThreadLink(threadUrl(target))).toEqual({kind:"thread",target});
  expect(threadReference(target)).toBe(`[Thread #0](${location.origin}/t/0?history=${history})`);
  expect(parseThreadLink(`/t/42?history=${history.toUpperCase()}`)).toMatchObject({target:{thread_id:42,history_id:history}});
 });
 it("does not claim remote or scheme-relative links",()=>{
  for(const url of [`https://github.com/t/2?history=${history}`,`https://example.test.evil/t/2?history=${history}`,`//${location.host}/t/2?history=${history}`,`javascript:/t/2?history=${history}`]) expect(parseThreadLink(url)).toBeNull();
 });
 it("makes unqualified links incomplete and rejects malformed identities",()=>{
  expect(parseThreadLink('/t/2')).toEqual({kind:'incomplete'});
  for(const path of [`/t/02?history=${history}`,`/t/-1?history=${history}`,`/t/9007199254740993?history=${history}`,`/t/2?history=old`,`/t/2?history=${history}&history=${history}`,`/t/2?history=`, `/t/2/extra?history=${history}`]) expect(parseThreadLink(path)).toEqual({kind:'invalid'});
 });
});
