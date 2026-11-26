import { delay as pause } from 'msw';
export async function handler() { await pause(250); }

