// Same projection as cli/api_schema.rs. The fixture test compares both outputs.
export function publicSchema(input) {
  const schema = structuredClone(input);
  function strip(v) {
    if (!v || typeof v !== 'object') return;
    if (Array.isArray(v)) return v.forEach(strip);
    for (const [k, child] of Object.entries(v)) {
      if (child?.['x-visibility'] === 'internal' || child?.['x-stability'] === 'internal') delete v[k];
    }
    if (typeof v.description === 'string') v.description = v.description.replace(/\(?\b(?:A:\d+|C1[ab]-\d+|[DQN]\d+)\)?|§\d+(?:\.\d+)*/g, '');
    Object.values(v).forEach(strip);
    if (v.required) v.required = v.required.filter((k) => Object.hasOwn(v.properties ?? {}, k));
  }
  strip(schema);
  const defs = schema.$defs;
  delete schema.$defs;
  const seen = new Set();
  function refs(v) {
    if (!v || typeof v !== 'object') return;
    if (v.$ref?.startsWith('#/$defs/')) {
      const name = v.$ref.slice(8);
      if (!seen.has(name)) { seen.add(name); refs(defs[name]); }
    }
    Object.values(v).forEach(refs);
  }
  refs(schema);
  schema.$defs = Object.fromEntries(Object.entries(defs).filter(([k]) => seen.has(k)));
  return schema;
}
