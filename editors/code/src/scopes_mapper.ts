import * as vscode from 'vscode';
import { TextMateRuleSettings } from './scopes';

let mappings = new Map<string, string[]>();

const defaultMapping = new Map<string, string[]>([
    [
        'comment',
        [
            'comment',
            'comment.block',
            'comment.line',
            'comment.block.documentation'
        ]
    ],
    ['string', ['string']],
    ['keyword', ['keyword']],
    ['keyword.control', ['keyword.control', 'keyword', 'keyword.other']],
    [
        'keyword.unsafe',
        ['storage.modifier', 'keyword.other', 'keyword.control', 'keyword']
    ],
    ['function', ['entity.name.function']],
    ['parameter', ['variable.parameter']],
    ['constant', ['constant', 'variable']],
    ['type', ['entity.name.type']],
    ['type.param', ['storage.type.generic', 'support.type']],
    ['builtin', ['variable.language', 'support.type']],
    ['text', ['string', 'string.quoted', 'string.regexp']],
    ['attribute', ['meta.attribute', 'keyword']],
    ['literal', ['string', 'string.quoted', 'string.regexp']],
    ['lifetime', ['entity.name.lifetime.rust', 'storage.modifier.lifetime.rust', 'support.type']],
    ['macro', ['support.macro.rust', 'support.other']],
    ['variable', ['variable']],
    ['variable.mut', ['variable.mut.rust', 'variable', 'storage.modifier']],
    [
        'field',
        [
            'variable.object.property',
            'meta.field.declaration',
            'meta.definition.property',
            'variable.other'
        ]
    ],
    ['module', ['entity.name.section', 'entity.other']]
]);

// Temporary exported for debugging for now.
export function find(scope: string): string[] {
    return mappings.get(scope) || [];
}

export function toRule(
    scope: string,
    intoRule: (scope: string) => TextMateRuleSettings | undefined
): TextMateRuleSettings | undefined {
    return find(scope)
        .map(intoRule)
        .filter(rule => rule !== undefined)[0];
}

function isString(value: any): value is string {
    return typeof value === 'string';
}

function isArrayOfString(value: any): value is string[] {
    return Array.isArray(value) && value.every(item => isString(item));
}

export function load() {
    const rawConfig: { [key: string]: any } =
        vscode.workspace
            .getConfiguration('rust-analyzer')
            .get('scopeMappings') || {};

    mappings = Object.entries(rawConfig)
        .filter(([_, value]) => isString(value) || isArrayOfString(value))
        .reduce((list, [key, value]: [string, string | string[]]) => {
            return list.set(key, isString(value) ? [value] : value);
        }, defaultMapping);
}
