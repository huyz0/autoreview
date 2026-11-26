import { delay as pause } from 'msw/utils/delay';
export async function handler() { await pause(250); }

