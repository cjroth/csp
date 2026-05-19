// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// https://astro.build/config
export default defineConfig({
	// Deploy URL — used for canonical links and the sitemap.
	// Change this to wherever the docs are hosted.
	site: 'https://cjroth.github.io/csp',
	integrations: [
		starlight({
			title: 'Context Sync Protocol',
			description:
				'CSP syncs agent context across tools, devices, and apps — plain files on disk, real-time sync, stock-git-compatible history, deterministic merge.',
			tagline: 'Sync agent context across tools, devices, and apps.',
			social: [
				{ icon: 'github', label: 'GitHub', href: 'https://github.com/cjroth/csp' },
			],
			customCss: ['./src/styles/global.css'],
			pagination: false,
			sidebar: [
				{
					label: 'Start here',
					items: [
						{ label: 'Introduction', link: '/' },
						{ label: 'Quick start', slug: 'quick-start' },
						{ label: 'Architecture', slug: 'architecture' },
					],
				},
				{
					label: 'Protocol',
					items: [
						{ label: 'Overview', slug: 'protocol/overview' },
						{ label: 'Design specification', slug: 'protocol/spec' },
					],
				},
				{
					label: 'Deploy',
					items: [{ label: 'Deploying a hub', slug: 'deploying' }],
				},
				{
					label: 'Rust Core · csp-core',
					items: [{ label: 'Overview', slug: 'rust-core/overview' }],
				},
				{
					label: 'CLI · ctx',
					items: [{ label: 'Overview', slug: 'cli/overview' }],
				},
				{
					label: 'SDK · wasm + TypeScript',
					items: [{ label: 'Overview', slug: 'sdk/overview' }],
				},
				{
					label: 'Obsidian Plugin',
					items: [
						{ label: 'Overview', slug: 'obsidian/overview' },
						{ label: 'Plugin specification', slug: 'obsidian/spec' },
					],
				},
				{
					label: 'Desktop App',
					items: [
						{ label: 'Overview', slug: 'desktop/overview' },
						{ label: 'App specification', slug: 'desktop/spec' },
					],
				},
			],
		}),
	],
});
