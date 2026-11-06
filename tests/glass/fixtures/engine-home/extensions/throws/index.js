// Test fixture: mount() throws. The host frame must keep working.
export function mount() {
    throw new Error('fixture: mount failed on purpose');
}
