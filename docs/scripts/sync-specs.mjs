// Regenerates the "deep specification" pages from the source spec markdown at
// the repo root. These pages are generated artifacts: edit the source specs
// (spec.md, obsidian-plugin-spec.md, desktop-app-spec.md), not the output.
//
// Runs automatically before `npm run build` (see package.json `prebuild`),
// and can be run on demand with `npm run sync:specs`.

import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, '../..');
const docsRoot = resolve(here, '..');

const jobs = [
	{
		src: 'spec.md',
		out: 'src/content/docs/protocol/spec.md',
		title: 'Context Sync Protocol — Design specification',
		description:
			'The complete CSP design: object model, deterministic fold/merge, identity, replication protocol, storage, security, and cross-surface conformance.',
	},
	{
		src: 'obsidian-plugin-spec.md',
		out: 'src/content/docs/obsidian/spec.md',
		title: 'Context for Obsidian — Design specification',
		description:
			'The complete Obsidian plugin design: thin-node model, module architecture, setup and connection flows, two-way materialization, snapshots, and parity tests.',
	},
	{
		src: 'desktop-app-spec.md',
		out: 'src/content/docs/desktop/spec.md',
		title: 'Context Desktop — Design specification',
		description:
			'The complete desktop app design: Tauri process/engine model, UI surfaces, per-folder listeners, and the UI-action to engine-operation mapping.',
	},
];

for (const { src, out, title, description } of jobs) {
	const raw = readFileSync(resolve(repoRoot, src), 'utf8');
	const lines = raw.split('\n');

	// Drop the leading "# …" H1 (Starlight renders the frontmatter title as the
	// page H1) and a single blank line immediately after it.
	if (lines[0]?.startsWith('# ')) {
		lines.shift();
		if (lines[0]?.trim() === '') lines.shift();
	}

	const frontmatter = [
		'---',
		`title: ${JSON.stringify(title)}`,
		`description: ${JSON.stringify(description)}`,
		'---',
		'',
	].join('\n');

	writeFileSync(resolve(docsRoot, out), frontmatter + lines.join('\n'));
	console.log(`sync-specs: ${src} → ${out}`);
}
