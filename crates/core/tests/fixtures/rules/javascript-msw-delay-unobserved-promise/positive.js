import { delay as pause } from 'msw';
export function handler() { pause(250); }

