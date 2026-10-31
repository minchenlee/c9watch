import { readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { generate } from './gen-c9p-types.mjs';
import { publicSchema } from './public-c9p-schema.mjs';
const root = new URL('../', import.meta.url);
export function sdkTypes() {
  const schema = publicSchema(JSON.parse(readFileSync(new URL('docs/api/c9p-1.json', root), 'utf8')));
  return generate(schema).replace('scripts/gen-c9p-types.mjs', 'scripts/gen-c9p-sdk.mjs')
    + `\nexport const C9P_PUBLIC_METHODS = ${JSON.stringify(Object.keys(schema.methods))} as const;\n`
    + `export const C9P_PUBLIC_TOPICS = ${JSON.stringify(Object.keys(schema.topics))} as const;\n`;
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  writeFileSync(new URL('packages/c9p-client/src/types.ts', root), sdkTypes());
  console.log('wrote packages/c9p-client/src/types.ts');
}
