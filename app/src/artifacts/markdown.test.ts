import { describe, expect, it } from "vitest";
import { artifactDocument } from "./document";
import type { Artifact } from "./types";
const file: Artifact = { id: 1, kind: "file", mime: "text/markdown", filename: "plan.md", title: "Plan", content: "", created_at: "a", updated_at: "a", thread_ids: [] };
const doc = (content: string, patch: Partial<Artifact> = {}) => new DOMParser().parseFromString(artifactDocument({ ...file, ...patch, content }), "text/html");
describe("explicit Markdown artifacts", () => {
  it("renders headings, lists, fenced code and checked links through the common parser", () => {
    const page = doc('# Plan\n\n- Read\n- Build\n\n```ts\nconst x = "<script>";\n```\n\n[Docs](https://example.org)');
    expect(page.querySelector('h1')?.textContent).toBe('Plan');
    expect(page.querySelectorAll('li')).toHaveLength(2);
    expect(page.querySelector('pre code')?.textContent).toBe('const x = "<script>";');
    expect(page.querySelector('.document-link')?.textContent).toBe('Docs (https://example.org)');
    expect(page.querySelector('a')).toBeNull();
  });
  it("keeps HTML literal, unsafe URLs inert and #references free of host controls", () => {
    const page = doc('<script>alert(1)</script>\n\n[x](javascript:alert) #42');
    expect(page.querySelector('.markdown')?.textContent).toContain('<script>alert(1)</script>');
    expect(page.querySelector('.markdown script')).toBeNull();
    expect(page.querySelector('.markdown a')).toBeNull();
    expect(page.querySelector('.markdown button')).toBeNull();
    expect(page.querySelector('meta[http-equiv]')?.getAttribute('content')).toContain("connect-src 'none'");
  });
  it("recognizes Markdown filenames and leaves ordinary text preformatted with Escape available", () => {
    expect(doc('# Filename', { mime: 'text/plain', filename: 'NOTES.MD' }).querySelector('h1')?.textContent).toBe('Filename');
    const plain = doc('# Plain', { mime:'text/plain',filename:'notes.txt' });
    expect(plain.querySelector('h1')).toBeNull();
    expect(plain.body.textContent).toContain('# Plain');
    expect(plain.querySelector('script')?.textContent).toContain("event.key === 'Escape'");
  });
});

it("keeps reference-link destinations visible and gives tables semantic headers", () => {
  const page=doc('[Guide][g]\n\n[g]: https://example.org/guide\n\n| Name | Value |\n| --- | --- |\n| A | B |');
  expect(page.querySelector('.document-link')?.textContent).toBe('Guide (https://example.org/guide)');
  expect(page.querySelectorAll('thead th')).toHaveLength(2);
  expect(page.querySelectorAll('tbody td')).toHaveLength(2);
  expect(page.querySelector('a')).toBeNull();
});

it("retains read-only checked state, nested tasks, continuation paragraphs and ordinary lists", () => {
  const page = doc('- [x] Complete **first** item\n- [ ] Incomplete item\n\n  Continuation paragraph.\n\n  - [x] Nested task\n  - Ordinary nested item\n\n- Ordinary outer item\n\n1. Numbered item\n\n- [ ] <script>unsafe()</script>');
  const checkboxes = [...page.querySelectorAll<HTMLInputElement>('.markdown input[type="checkbox"]')];
  expect(checkboxes.map(input => input.checked)).toEqual([true,false,true,false]);
  expect(checkboxes.every(input => input.disabled)).toBe(true);
  expect(page.querySelectorAll('.markdown ol li')).toHaveLength(1);
  expect(page.querySelector('.markdown')?.textContent).toContain('Continuation paragraph.');
  expect(page.querySelector('.markdown')?.textContent).toContain('Ordinary nested item');
  expect(page.querySelector('.markdown')?.textContent).toContain('Ordinary outer item');
  expect(page.querySelector('.markdown')?.textContent).toContain('<script>unsafe()</script>');
  expect(page.querySelector('.markdown script')).toBeNull();
});
