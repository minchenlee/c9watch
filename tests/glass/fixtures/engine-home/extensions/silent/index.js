// Test fixture: mounts but never calls host.ready(), and its promise never
// settles. The host shows 「extension 沒有回應」 after 5 s.
/** @param {ShadowRoot} root */
export function mount(root) {
    root.innerHTML = '<p>silent</p>';
    return new Promise(() => {});
}
