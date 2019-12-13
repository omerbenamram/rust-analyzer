import * as vscode from 'vscode';
import * as scopes from './scopes';
import * as scopesMapper from './scopes_mapper';
import { Server } from './server';

const RA_LSP_DEBUG = process.env.__RA_LSP_SERVER_DEBUG;

export type CargoWatchStartupOptions = 'ask' | 'enabled' | 'disabled';
export type CargoWatchTraceOptions = 'off' | 'error' | 'verbose';

export interface CargoWatchOptions {
    enableOnStartup: CargoWatchStartupOptions;
    arguments: string;
    command: string;
    trace: CargoWatchTraceOptions;
    ignore: string[];
}

export class Config {
    public highlightingOn = true;
    public rainbowHighlightingOn = false;
    public enableEnhancedTyping = true;
    public raLspServerPath = RA_LSP_DEBUG || 'ra_lsp_server';
    public lruCapacity: null | number = null;
    public displayInlayHints = true;
    public maxInlayHintLength: null | number = null;
    public excludeGlobs = [];
    public useClientWatching = false;
    public featureFlags = {};
    // for internal use
    public withSysroot: null | boolean = null;
    public cargoWatchOptions: CargoWatchOptions = {
        enableOnStartup: 'ask',
        trace: 'off',
        arguments: '',
        command: '',
        ignore: [],
    };

    private prevEnhancedTyping: null | boolean = null;

    constructor() {
        vscode.workspace.onDidChangeConfiguration(_ =>
            this.userConfigChanged(),
        );
        this.userConfigChanged();
    }

    public userConfigChanged() {
        const config = vscode.workspace.getConfiguration('rust-analyzer');

        Server.highlighter.removeHighlights();

        if (config.has('highlightingOn')) {
            this.highlightingOn = config.get('highlightingOn') as boolean;
            if (this.highlightingOn) {
                scopes.load();
                scopesMapper.load();
            }
        }

        if (config.has('rainbowHighlightingOn')) {
            this.rainbowHighlightingOn = config.get(
                'rainbowHighlightingOn',
            ) as boolean;
        }

        if (config.has('enableEnhancedTyping')) {
            this.enableEnhancedTyping = config.get(
                'enableEnhancedTyping',
            ) as boolean;

            if (this.prevEnhancedTyping === null) {
                this.prevEnhancedTyping = this.enableEnhancedTyping;
            }
        } else if (this.prevEnhancedTyping === null) {
            this.prevEnhancedTyping = this.enableEnhancedTyping;
        }

        if (this.prevEnhancedTyping !== this.enableEnhancedTyping) {
            const reloadAction = 'Reload now';
            vscode.window
                .showInformationMessage(
                    'Changing enhanced typing setting requires a reload',
                    reloadAction,
                )
                .then(selectedAction => {
                    if (selectedAction === reloadAction) {
                        vscode.commands.executeCommand(
                            'workbench.action.reloadWindow',
                        );
                    }
                });
            this.prevEnhancedTyping = this.enableEnhancedTyping;
        }

        if (config.has('raLspServerPath')) {
            this.raLspServerPath =
                RA_LSP_DEBUG || (config.get('raLspServerPath') as string);
        }

        if (config.has('enableCargoWatchOnStartup')) {
            this.cargoWatchOptions.enableOnStartup = config.get<
                CargoWatchStartupOptions
            >('enableCargoWatchOnStartup', 'ask');
        }

        if (config.has('trace.cargo-watch')) {
            this.cargoWatchOptions.trace = config.get<CargoWatchTraceOptions>(
                'trace.cargo-watch',
                'off',
            );
        }

        if (config.has('cargo-watch.arguments')) {
            this.cargoWatchOptions.arguments = config.get<string>(
                'cargo-watch.arguments',
                '',
            );
        }

        if (config.has('cargo-watch.command')) {
            this.cargoWatchOptions.command = config.get<string>(
                'cargo-watch.command',
                '',
            );
        }

        if (config.has('cargo-watch.ignore')) {
            this.cargoWatchOptions.ignore = config.get<string[]>(
                'cargo-watch.ignore',
                [],
            );
        }

        if (config.has('lruCapacity')) {
            this.lruCapacity = config.get('lruCapacity') as number;
        }

        if (config.has('displayInlayHints')) {
            this.displayInlayHints = config.get('displayInlayHints') as boolean;
        }
        if (config.has('maxInlayHintLength')) {
            this.maxInlayHintLength = config.get(
                'maxInlayHintLength',
            ) as number;
        }
        if (config.has('excludeGlobs')) {
            this.excludeGlobs = config.get('excludeGlobs') || [];
        }
        if (config.has('useClientWatching')) {
            this.useClientWatching = config.get('useClientWatching') || false;
        }
        if (config.has('featureFlags')) {
            this.featureFlags = config.get('featureFlags') || {};
        }
        if (config.has('withSysroot')) {
            this.withSysroot = config.get('withSysroot') || false;
        }
    }
}
