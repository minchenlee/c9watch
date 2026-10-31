import { writable } from 'svelte/store';
import type { DetectionDiagnostics } from '../../types';
import { getDetectionDiagnostics } from '../api';
export const diagnostics = writable<DetectionDiagnostics | null>(null);
export async function refreshDiagnostics() { diagnostics.set(await getDetectionDiagnostics()); }
