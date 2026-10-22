// Golden checks for src/lib/markdown.ts. DOMPurify has no DOM under Node, so
// these tests compare marked's output before sanitizing. The reference
// functions are verbatim copies of the inline renderers the components used
// before the shared module existed (MessageBubble, MemoryViewer, SettingsTab).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { marked } from 'marked';
import { markdownToHtml, plainMarkdownToHtml } from '../src/lib/markdown.ts';

function referenceMessageBubble(content) {
	const renderer = new marked.Renderer();
	renderer.code = function ({ text, lang }) {
		const language = lang || 'code';
		return `
				<div class="code-block-wrapper">
					<div class="code-header">
						<span class="code-lang">${language}</span>
						<div class="code-actions">
							<span class="code-dot"></span>
							<span class="code-dot"></span>
						</div>
					</div>
					<pre><code class="language-${language}">${text}</code></pre>
				</div>
			`;
	};
	return marked.parse(content, { async: false, breaks: true, gfm: true, renderer });
}

function referenceMemoryViewer(content) {
	const renderer = new marked.Renderer();
	renderer.code = function ({ text, lang }) {
		const language = lang || 'code';
		return `
				<div class="code-block-wrapper">
					<div class="code-header">
						<span class="code-lang">${language}</span>
					</div>
					<pre><code class="language-${language}">${text}</code></pre>
				</div>
			`;
	};
	return marked.parse(content, { async: false, breaks: true, gfm: true, renderer });
}

function referenceSettingsTab(content) {
	return marked.parse(content, { async: false, breaks: true, gfm: true });
}

const samples = [
	'',
	'plain text',
	'line one\nline two',
	'# Heading\n\n- item **bold**\n- [link](https://example.com)',
	'```ts\nconst a = 1 < 2;\n```',
	'```\nno language\n```',
	'before\n\n```rust\nfn main() {}\n```\n\nafter `inline` code',
	'| a | b |\n|---|---|\n| 1 | 2 |',
	'    indented code block',
	'<b>raw html</b> & ~~strike~~'
];

test('codeDots matches the MessageBubble renderer', () => {
	for (const s of samples) assert.equal(markdownToHtml(s, { codeDots: true }), referenceMessageBubble(s), s);
});

test('default options match the MemoryViewer renderer', () => {
	for (const s of samples) assert.equal(markdownToHtml(s), referenceMemoryViewer(s), s);
});

test('plain renderer matches the SettingsTab renderer', () => {
	for (const s of samples) assert.equal(plainMarkdownToHtml(s), referenceSettingsTab(s), s);
});

test('code blocks carry the expected wrapper and actions', () => {
	const md = '```js\nx\n```';
	assert.match(markdownToHtml(md, { codeDots: true }), /<div class="code-actions">/);
	assert.doesNotMatch(markdownToHtml(md), /code-actions/);
	assert.match(markdownToHtml(md), /<code class="language-js">x<\/code>/);
});

test('shared renderers give the same result on repeated calls', () => {
	for (const s of samples) {
		assert.equal(markdownToHtml(s, { codeDots: true }), markdownToHtml(s, { codeDots: true }));
		assert.equal(markdownToHtml(s), markdownToHtml(s));
	}
});
