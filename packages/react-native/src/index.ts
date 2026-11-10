export * from './types.js';
export { LiliaDatabase } from './database.js';
export { DatabaseObserver } from './observer.js';
export { resolveDatabasePath } from './sandbox.js';
export { LiliaContext, useDatabase, useLilia, LiliaConsumer } from './context.js';
export { LiliaProvider } from './provider.js';
export {
  useLiliaDatabase,
  useKv,
  useKvState,
  useDocument,
  useJson,
  useGraphNeighbors,
  useFts,
  useSqlQuery,
  useMutation,
  useDatabaseMaintenance,
} from './hooks.js';

// ==========================================
// BUILT-IN IDIOMATIC MOBILE COMPONENTS
// ==========================================

// Lists
export { LiliaFlatList } from './components/LiliaFlatList.js';
export { LiliaFlashList, type LiliaFlashListProps } from './components/lists/LiliaFlashList.js';
export {
  LiliaSectionList,
  type LiliaSectionListProps,
  type LiliaSectionData,
} from './components/lists/LiliaSectionList.js';
export {
  LiliaInfiniteList,
  type LiliaInfiniteListProps,
} from './components/lists/LiliaInfiniteList.js';

// KV Zero-Boilerplate
export { LiliaKvSwitch, type LiliaKvSwitchProps } from './components/kv/LiliaKvSwitch.js';
export { LiliaKvInput, type LiliaKvInputProps } from './components/kv/LiliaKvInput.js';

// Document & Forms
export {
  LiliaDocumentView,
  type LiliaDocumentViewProps,
} from './components/document/LiliaDocumentView.js';
export {
  LiliaForm,
  type LiliaFormProps,
  type FormHelpers,
} from './components/document/LiliaForm.js';

// Feedback, States & Error Boundaries
export {
  LiliaCountBadge,
  type LiliaCountBadgeProps,
} from './components/feedback/LiliaCountBadge.js';
export {
  LiliaErrorBoundary,
  type LiliaErrorBoundaryProps,
} from './components/feedback/LiliaErrorBoundary.js';
export {
  LiliaSuspense,
  type LiliaSuspenseProps,
} from './components/feedback/LiliaSuspense.js';
export {
  LiliaOptimisticButton,
  type LiliaOptimisticButtonProps,
} from './components/feedback/LiliaOptimisticButton.js';

// Full-Text Search (FTS5)
export {
  LiliaSearchBar,
  type LiliaSearchBarProps,
} from './components/fts/LiliaSearchBar.js';
export {
  LiliaHighlightSnippet,
  type LiliaHighlightSnippetProps,
} from './components/fts/LiliaHighlightSnippet.js';

// Graph Exploration
export {
  LiliaGraphExplorer,
  type LiliaGraphExplorerProps,
} from './components/graph/LiliaGraphExplorer.js';
export {
  LiliaGraphBreadcrumbs,
  type LiliaGraphBreadcrumbsProps,
} from './components/graph/LiliaGraphBreadcrumbs.js';

// Hardware Security
export {
  LiliaBiometricLock,
  type LiliaBiometricLockProps,
} from './components/security/LiliaBiometricLock.js';
export {
  LiliaSecureField,
  type LiliaSecureFieldProps,
} from './components/security/LiliaSecureField.js';

// Storage & Maintenance
export {
  LiliaStorageBar,
  type LiliaStorageBarProps,
} from './components/storage/LiliaStorageBar.js';
export {
  LiliaBackupShareButton,
  type LiliaBackupShareButtonProps,
} from './components/storage/LiliaBackupShareButton.js';

// DevTools
export {
  LiliaDevInspector,
  type LiliaDevInspectorProps,
} from './components/devtools/LiliaDevInspector.js';
