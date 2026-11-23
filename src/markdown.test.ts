import { expect, test } from "bun:test";
import { renderMarkdown } from "./markdown";

test("shows raw HTML as text instead of markup", () => {
  const html = renderMarkdown('<script>alert(1)</script>\n\nHi <img src=x onerror="alert(1)"> and <a href="javascript:alert(1)">x</a>');
  expect(html).not.toContain("<script");
  expect(html).not.toContain("<img");
  expect(html).not.toContain('<a href="javascript');
  expect(html).toContain("&lt;script&gt;alert(1)&lt;/script&gt;");
  expect(html).toContain("&lt;img src=x onerror=&quot;alert(1)&quot;&gt;");
});

test("keeps web and mail links and drops every other scheme", () => {
  expect(renderMarkdown("[docs](https://example.com/a?b=1&c=2)")).toContain('<a href="https://example.com/a?b=1&amp;c=2">docs</a>');
  expect(renderMarkdown("[mail](mailto:me@example.com)")).toContain('<a href="mailto:me@example.com">mail</a>');
  for (const href of ["javascript:alert(1)", "JaVaScRiPt:alert(1)", "jav&#x61;script:alert(1)", "data:text/html,<b>x</b>", "vbscript:x", "file:///etc/passwd", "/relative"]) {
    const html = renderMarkdown(`[click](${href})`);
    expect(html).not.toContain("<a");
    expect(html).toContain("click");
  }
});

test("keeps quotes in a link from breaking out of the href", () => {
  const html = renderMarkdown('[x](https://example.com/"onmouseover="alert(1))');
  expect(html).toContain('<a href="https://example.com/&quot;onmouseover=&quot;alert(1)">x</a>');
});

test("shows images as their alt text and never loads them", () => {
  const html = renderMarkdown('![a <b>cat</b>](https://example.com/cat.png) ![x" onerror="alert(1)](javascript:alert(1))');
  expect(html).not.toContain("<img");
  expect(html).not.toContain("example.com/cat.png");
  expect(html).toContain("a &lt;b&gt;cat&lt;/b&gt;");
  expect(html).not.toContain('" onerror');
});
