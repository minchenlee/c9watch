export interface FormOption { value: string; label: string }
export interface FormField {
 type: 'string' | 'number' | 'integer' | 'boolean' | 'array';
 title?: string; description?: string; enum?: string[]; enumNames?: string[];
 oneOf?: { const: string; title?: string }[]; anyOf?: { const: string; title?: string }[];
 items?: FormField; minLength?: number; maxLength?: number;
 minimum?: number; maximum?: number; minItems?: number; maxItems?: number; format?: string;
}
export interface FormSchema { type: string; properties: Record<string, FormField>; required?: string[] }
export function formOptions(field: FormField): FormOption[] {
 const options = field.type === 'array' ? field.items : field;
 if (!options) return [];
 if (options.enum) return options.enum.map((value, i) => ({ value, label: options.enumNames?.[i] ?? value }));
 return (options.oneOf ?? options.anyOf ?? []).map(o => ({ value: o.const, label: o.title ?? o.const }));
}
export function safeExternalUrl(value: string): boolean {
 try { const u = new URL(value); return ['https:', 'http:'].includes(u.protocol) && !!u.hostname && !u.username && !u.password; }
 catch { return false; }
}
export function formComplete(schema: FormSchema | undefined, values: Record<string, unknown>): boolean {
 return !!schema && (schema.required ?? []).every(key => Object.hasOwn(values, key));
}
