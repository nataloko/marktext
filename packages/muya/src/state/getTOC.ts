import type { Muya } from '../muya';
import type { TState } from './types';
import { tokenizer, tokensToInlineHtml, tokensToPlainText } from '../inlineRenderer/lexer';
import { getUniqueId } from '../utils';
import { generateGithubSlug } from '../utils/slug';
import { isAtxHeadingState, isSetextHeadingState } from './types';

export interface ITocItem {
    content: string;
    contentHtml: string;
    lvl: number;
    slug: string;
    githubSlug: string;
    // Top-level state index of the heading. During a progressive mount the
    // heading's DOM may not exist yet; hosts pass this to
    // `muya.ensureMountedThrough(index)` before scrolling to it.
    index: number;
}

// Slugs are keyed on the heading's STATE node, not its block: the TOC is
// collected from `jsonState` (the complete logical document) so it stays
// correct even while block mounting is in progress, and block-side consumers
// (headingCopyLink) resolve their heading to the same state node. ot-json1
// apply is copy-on-write, so an unedited heading keeps its node — and its
// slug — across other edits; editing the heading replaces the node, which
// only re-keys that entry (hosts rebuild their TOC on `json-change` anyway).
const slugCache = new WeakMap<object, string>();

export function stableSlug(node: object): string {
    let slug = slugCache.get(node);
    if (slug == null) {
        slug = getUniqueId();
        slugCache.set(node, slug);
    }
    return slug;
}

export function getTOC(muya: Muya): ITocItem[] {
    const states: readonly TState[] = muya.editor.jsonState.rawState;

    // Headings are tokenized outside a rendered block, so the reference
    // definitions a heading may link to (`[text][ref]`) have to be collected
    // here instead of from the block's own inline renderer.
    const labels = muya.editor.inlineRenderer.collectReferenceDefinitions();

    const items: ITocItem[] = [];

    for (let index = 0; index < states.length; index++) {
        const state = states[index];
        if (!isAtxHeadingState(state) && !isSetextHeadingState(state))
            continue;

        const text = state.text ?? '';
        const source = isSetextHeadingState(state)
            ? text.trim()
            : text.replace(/^\s*#{1,6}\s+/, '').trim();

        // One token pass feeds both serializations so the outline's shown text
        // and its slug stay in step; `githubSlug` has to match the anchor id the
        // HTML export injects from `heading.textContent`.
        const { superSubScript, footnote, texMathDollars, texMathGfm, texMathSingleBackslash, texMathDoubleBackslash, highlightSyntax, inlineDiff } = muya.options;
        const tokens = tokenizer(source, {
            hasBeginRules: false,
            labels,
            options: { superSubScript, footnote, texMathDollars, texMathGfm, texMathSingleBackslash, texMathDoubleBackslash, highlightSyntax, inlineDiff },
        });

        const content = tokensToPlainText(tokens).trim();
        const contentHtml = tokensToInlineHtml(tokens).trim();

        items.push({
            content,
            contentHtml,
            lvl: state.meta.level,
            slug: stableSlug(state),
            githubSlug: generateGithubSlug(content),
            index,
        });
    }

    return items;
}
