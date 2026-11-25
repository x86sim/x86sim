import { answer } from './mod.mjs';
postMessage({ answer: answer(), module: !!import.meta.url });
