import { marked } from 'marked';
import DOMPurify from 'dompurify';

export interface MarkdownOptions {
	/** Add the two decorative dots (`.code-actions`) to each code block header. */
	codeDots?: boolean;
}

// The template keeps the exact whitespace the components used to emit inline.
const CODE_ACTIONS = `						<div class="code-actions">
							<span class="code-dot"></span>
							<span class="code-dot"></span>
						</div>
`;

function createCodeRenderer(codeDots: boolean) {
	const renderer = new marked.Renderer();
	const actions = codeDots ? CODE_ACTIONS : '';
	renderer.code = function ({ text, lang }) {
		const language = lang || 'code';
		return `
				<div class="code-block-wrapper">
					<div class="code-header">
						<span class="code-lang">${language}</span>
${actions}					</div>
					<pre><code class="language-${language}">${text}</code></pre>
				</div>
			`;
	};
	return renderer;
}

const codeRenderer = createCodeRenderer(false);
const codeRendererWithDots = createCodeRenderer(true);

/** Markdown to HTML with the custom code block renderer, before sanitizing. */
export function markdownToHtml(content: string, options: MarkdownOptions = {}): string {
	return marked.parse(content, {
		async: false,
		breaks: true,
		gfm: true,
		renderer: options.codeDots ? codeRendererWithDots : codeRenderer
	}) as string;
}

/** Markdown to HTML with marked's default renderer, before sanitizing. */
export function plainMarkdownToHtml(content: string): string {
	return marked.parse(content, { async: false, breaks: true, gfm: true }) as string;
}

/** Sanitized HTML for message bubbles and memory files (custom code blocks). */
export function renderMarkdown(content: string, options: MarkdownOptions = {}): string {
	return DOMPurify.sanitize(markdownToHtml(content, options));
}

/** Sanitized HTML using marked's default renderer (release notes). */
export function renderPlainMarkdown(content: string): string {
	return DOMPurify.sanitize(plainMarkdownToHtml(content));
}
