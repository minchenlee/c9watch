<script lang="ts">
 import { formOptions, type FormSchema } from '$lib/codex-forms';
 let { schema, values = $bindable({}), disabled = false }: { schema: FormSchema; values?: Record<string, unknown>; disabled?: boolean } = $props();
 function set(key: string, value: unknown) {
  values = Object.fromEntries([...Object.entries(values).filter(([name]) => name !== key), ...(value === undefined ? [] : [[key, value]])]);
 }
 function toggle(key: string, value: string) {
  const current = (values[key] as string[] | undefined) ?? [];
  set(key, current.includes(value) ? current.filter(v => v !== value) : [...current, value]);
 }
</script>
{#each Object.entries(schema.properties) as [key, field]}
 <fieldset {disabled}>
  <legend>{field.title ?? key}{#if schema.required?.includes(key)} <small>required</small>{/if}</legend>
  {#if field.description}<p>{field.description}</p>{/if}
  {#if field.type === 'boolean'}
   {#each [true, false] as value}<button class:selected={values[key] === value} aria-pressed={values[key] === value} onclick={() => set(key, value)}>{value ? 'Yes' : 'No'}</button>{/each}
  {:else if formOptions(field).length}
   {#each formOptions(field) as option}
    {@const selected = field.type === 'array' ? ((values[key] as string[] | undefined) ?? []).includes(option.value) : values[key] === option.value}
    <button class="option" class:selected aria-pressed={selected} onclick={() => field.type === 'array' ? toggle(key, option.value) : set(key, option.value)}><span class="marker"></span>{option.label}</button>
   {/each}
   {#if field.type === 'array'}<small>Select {field.minItems ?? 0}–{field.maxItems ?? formOptions(field).length} options</small>{/if}
  {:else if field.type === 'number' || field.type === 'integer'}
   <input type="number" aria-label={field.title ?? key} min={field.minimum} max={field.maximum} step={field.type === 'integer' ? 1 : 'any'} value={(values[key] as number | undefined) ?? ''} oninput={e => set(key, e.currentTarget.value === '' ? undefined : Number(e.currentTarget.value))} />
  {:else}
   <input type={field.format === 'email' ? 'email' : 'text'} aria-label={field.title ?? key} placeholder={field.format ?? ''} minlength={field.minLength} maxlength={Math.min(field.maxLength ?? 32768, 32768)} value={(values[key] as string | undefined) ?? ''} oninput={e => set(key, e.currentTarget.value === '' && !schema.required?.includes(key) ? undefined : e.currentTarget.value)} />
  {/if}
  {#if !schema.required?.includes(key) && Object.hasOwn(values, key)}<button class="clear" onclick={() => set(key, undefined)}>CLEAR OPTIONAL FIELD</button>{/if}
 </fieldset>
{/each}
<style>
 fieldset { border: 0; padding: 0; margin: 12px 0; min-width: 0; }
 legend { font-size: 13px; overflow-wrap: anywhere; }
 p, small { color: var(--text-secondary); font-size: 11px; }
 input, button { box-sizing: border-box; border: 1px solid var(--border-default); border-radius: 0; background: transparent; color: var(--text-primary); padding: 8px; font: 12px var(--font-mono); }
 input { width: 100%; margin-top: 6px; }
 button { cursor: pointer; margin: 4px 4px 0 0; }
 .option { display: flex; gap: 8px; align-items: center; width: 100%; text-align: left; overflow-wrap: anywhere; }
 .marker { flex-shrink: 0; width: 8px; height: 8px; border: 1px solid var(--border-default); }
 .selected { border-color: var(--text-primary); }
 .selected .marker { background: var(--text-primary); box-shadow: inset 0 0 0 2px var(--bg-card); }
 button:disabled, input:disabled { opacity: .5; cursor: default; }
 .clear { display: block; font-size: 10px; }
 :focus-visible { outline: 1px solid var(--text-primary); outline-offset: 2px; }
</style>
