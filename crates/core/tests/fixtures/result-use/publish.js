import {rename} from 'node:fs/promises';

export async function abandoned(from, to) {
    const pending = rename(from, to); // unused result; then await pending;
    const callback = async () => { await pending; };
}

export async function overwrittenField(from, to, choose) {
    const slot = {pending: null};
    slot.pending = rename(from, to); // unused result; then await slot.pending;
    if (choose) {
        slot.pending = Promise.resolve();
    } else {
        slot.pending = null;
    }
    await slot.pending;
}

export async function throughCallback(from, to) {
    const value = rename(from, to);
    const expose = () => value;
    const copy = expose();
    await copy;
}

export async function throughAsyncCallback(from, to) {
    const task = rename(from, to);
    const useTask = async () => { await task; };
    await useTask();
}

export async function replacedByCallback(from, to) {
    let obsolete = rename(from, to); // unused result; then await obsolete;
    const reset = () => { obsolete = Promise.resolve(); };
    reset();
    await obsolete;
}

export function escapingOwner(from, to) {
    const holder = {promise: null};
    holder.promise = rename(from, to);
    return holder;
}

export function callbackEscapes(from, to) {
    let delayed = Promise.resolve();
    const verify = () => delayed;
    const handle = schedule(verify);
    delayed = rename(from, to);
    return handle;
}

export async function fieldReadByCallback(from, to) {
    const box = {task: null};
    box.task = rename(from, to);
    const readTask = () => box.task;
    await readTask();
}
