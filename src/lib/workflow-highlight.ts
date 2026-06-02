// Minimal hand-rolled JS syntax highlighter for the workflow SCRIPT panel.
// No dependency — a single-pass tokenizer over the common JS lexical classes:
// line/block comments, strings (', ", `), numbers, keywords, and the rest as
// plain text. Output is HTML with <span class="tok-*"> wrappers; the input is
// HTML-escaped so it is safe to render with {@html}. Not a full parser — good
// enough for readability of a workflow script.

const KEYWORDS = new Set([
	'const', 'let', 'var', 'function', 'return', 'await', 'async', 'for', 'of',
	'in', 'if', 'else', 'while', 'do', 'switch', 'case', 'break', 'continue',
	'new', 'import', 'from', 'export', 'default', 'try', 'catch', 'finally',
	'throw', 'typeof', 'instanceof', 'this', 'class', 'extends', 'super',
	'true', 'false', 'null', 'undefined', 'void', 'yield',
]);

function escapeHtml(s: string): string {
	return s
		.replace(/&/g, '&amp;')
		.replace(/</g, '&lt;')
		.replace(/>/g, '&gt;');
}

function span(cls: string, text: string): string {
	return `<span class="tok-${cls}">${escapeHtml(text)}</span>`;
}

export function highlightJs(src: string): string {
	let out = '';
	let i = 0;
	const n = src.length;
	const isIdentStart = (c: string) => /[A-Za-z_$]/.test(c);
	const isIdent = (c: string) => /[A-Za-z0-9_$]/.test(c);
	const isDigit = (c: string) => /[0-9]/.test(c);

	while (i < n) {
		const c = src[i];

		// line comment
		if (c === '/' && src[i + 1] === '/') {
			let j = i + 2;
			while (j < n && src[j] !== '\n') j++;
			out += span('comment', src.slice(i, j));
			i = j;
			continue;
		}
		// block comment
		if (c === '/' && src[i + 1] === '*') {
			let j = i + 2;
			while (j < n && !(src[j] === '*' && src[j + 1] === '/')) j++;
			j = Math.min(j + 2, n);
			out += span('comment', src.slice(i, j));
			i = j;
			continue;
		}
		// string (', ", `) — track escapes; templates not deeply parsed
		if (c === '"' || c === "'" || c === '`') {
			let j = i + 1;
			while (j < n) {
				if (src[j] === '\\') {
					j += 2;
					continue;
				}
				if (src[j] === c) {
					j++;
					break;
				}
				j++;
			}
			out += span('string', src.slice(i, j));
			i = j;
			continue;
		}
		// number
		if (isDigit(c)) {
			let j = i + 1;
			while (j < n && /[0-9._eExXa-fA-F]/.test(src[j])) j++;
			out += span('number', src.slice(i, j));
			i = j;
			continue;
		}
		// identifier / keyword
		if (isIdentStart(c)) {
			let j = i + 1;
			while (j < n && isIdent(src[j])) j++;
			const word = src.slice(i, j);
			out += KEYWORDS.has(word) ? span('keyword', word) : escapeHtml(word);
			i = j;
			continue;
		}
		// everything else
		out += escapeHtml(c);
		i++;
	}
	return out;
}
