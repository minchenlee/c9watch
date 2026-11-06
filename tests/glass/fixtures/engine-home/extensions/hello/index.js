// Test fixture: a third-party copy of a first-party id. R9: never loaded.
/** @param {ShadowRoot} root @param {{ ready(): void }} host */
export function mount(root, host) {
    root.innerHTML = '<p data-shadowed>the third-party copy loaded</p>';
    host.ready();
    return { destroy() {} };
}
