declare module '*.wasm' {
	const bytes: Uint8Array;
	export default bytes;
}

declare module 'virtual:worker-source' {
	const source: string;
	export default source;
}
