import React, { useState, useCallback } from 'react';
import {
  View,
  Text,
  TextInput,
  TouchableOpacity,
  Modal,
  ScrollView,
  SafeAreaView,
  ActivityIndicator,
  StyleSheet,
  type StyleProp,
  type ViewStyle,
} from 'react-native';
import { getDefaultDatabase, type LiliaDatabase } from '../../database.js';
import { useDatabase } from '../../context.js';
import { LiliaStorageBar } from '../storage/LiliaStorageBar.js';

export interface LiliaDevInspectorProps {
  enabled?: boolean;
  position?: 'bottom-right' | 'bottom-left' | 'top-right' | 'top-left';
  db?: LiliaDatabase | null;
}

type TabType = 'sql' | 'storage' | 'kv';

/**
 * Idiomatic mobile DevTools floating inspector for LiliaDB.
 * Provides an on-device SQL console, live storage telemetry, and KV inspector in __DEV__.
 */
export function LiliaDevInspector({
  enabled = typeof __DEV__ !== 'undefined' ? __DEV__ : true,
  position = 'bottom-right',
  db: explicitDb,
}: LiliaDevInspectorProps): React.ReactElement | null {
  const [isOpen, setIsOpen] = useState(false);
  const [activeTab, setActiveTab] = useState<TabType>('sql');

  // SQL Console State
  const [sqlQuery, setSqlQuery] = useState('SELECT 1 as status, "Hello LiliaDB" as msg;');
  const [sqlResults, setSqlResults] = useState<{ columns: string[]; rows: any[]; rowsAffected?: number } | null>(null);
  const [sqlError, setSqlError] = useState<string | null>(null);
  const [isExecutingSql, setIsExecutingSql] = useState(false);

  // KV Inspector State
  const [kvNamespace, setKvNamespace] = useState('app_settings');
  const [kvKey, setKvKey] = useState('theme');
  const [kvValue, setKvValue] = useState<string | null>(null);
  const [kvStatus, setKvStatus] = useState<string | null>(null);

  if (!enabled) {
    return null;
  }

  let resolvedDb: LiliaDatabase | null = null;
  try {
    resolvedDb = useDatabase(explicitDb);
  } catch {
    resolvedDb = explicitDb ?? getDefaultDatabase();
  }

  const getDb = (): LiliaDatabase | null => {
    return resolvedDb;
  };

  const handleRunSql = async () => {
    const db = getDb();
    if (!db) {
      setSqlError('No active LiliaDB instance found.');
      return;
    }

    setIsExecutingSql(true);
    setSqlError(null);
    try {
      // Use synchronous JSI if available, fallback to async
      let res;
      if (db.sync?.sql?.query) {
        res = db.sync.sql.query(sqlQuery);
      } else {
        res = await db.sql.query(sqlQuery);
      }
      setSqlResults(res);
    } catch (err) {
      setSqlError(err instanceof Error ? err.message : String(err));
    } finally {
      setIsExecutingSql(false);
    }
  };

  const handleKvGet = () => {
    const db = getDb();
    if (!db) return;
    try {
      const val = db.sync?.kv?.peek(kvNamespace, kvKey);
      setKvValue(val ?? '<nil>');
      setKvStatus('Read completed');
    } catch (err) {
      setKvStatus(`Error: ${String(err)}`);
    }
  };

  const handleKvSet = async () => {
    const db = getDb();
    if (!db || !kvValue) return;
    try {
      db.sync?.kv?.set(kvNamespace, kvKey, kvValue);
      await db.kv.set(kvNamespace, kvKey, kvValue);
      setKvStatus('Saved successfully');
    } catch (err) {
      setKvStatus(`Save error: ${String(err)}`);
    }
  };

  const getBubblePositionStyle = (): ViewStyle => {
    switch (position) {
      case 'bottom-left':
        return { bottom: 24, left: 16 };
      case 'top-right':
        return { top: 60, right: 16 };
      case 'top-left':
        return { top: 60, left: 16 };
      case 'bottom-right':
      default:
        return { bottom: 24, right: 16 };
    }
  };

  return (
    <>
      {/* Floating Trigger Bubble */}
      <TouchableOpacity
        style={[styles.bubble, getBubblePositionStyle()]}
        onPress={() => setIsOpen(true)}
        activeOpacity={0.8}
      >
        <Text style={styles.bubbleText}>⚡ Lilia</Text>
      </TouchableOpacity>

      {/* Inspector Modal */}
      <Modal visible={isOpen} animationType="slide" onRequestClose={() => setIsOpen(false)}>
        <SafeAreaView style={styles.modalContainer}>
          {/* Header */}
          <View style={styles.modalHeader}>
            <View>
              <Text style={styles.modalTitle}>LiliaDB Dev Inspector</Text>
              <Text style={styles.modalSubtitle} numberOfLines={1}>
                {getDb()?.path ?? 'In-Memory / Default'}
              </Text>
            </View>
            <TouchableOpacity onPress={() => setIsOpen(false)} style={styles.closeButton}>
              <Text style={styles.closeButtonText}>✕</Text>
            </TouchableOpacity>
          </View>

          {/* Tab Navigation */}
          <View style={styles.tabBar}>
            <TouchableOpacity
              style={[styles.tab, activeTab === 'sql' && styles.activeTab]}
              onPress={() => setActiveTab('sql')}
            >
              <Text style={[styles.tabText, activeTab === 'sql' && styles.activeTabText]}>
                SQL Console
              </Text>
            </TouchableOpacity>

            <TouchableOpacity
              style={[styles.tab, activeTab === 'storage' && styles.activeTab]}
              onPress={() => setActiveTab('storage')}
            >
              <Text style={[styles.tabText, activeTab === 'storage' && styles.activeTabText]}>
                Storage
              </Text>
            </TouchableOpacity>

            <TouchableOpacity
              style={[styles.tab, activeTab === 'kv' && styles.activeTab]}
              onPress={() => setActiveTab('kv')}
            >
              <Text style={[styles.tabText, activeTab === 'kv' && styles.activeTabText]}>
                KV Store
              </Text>
            </TouchableOpacity>
          </View>

          {/* Tab Contents */}
          <ScrollView style={styles.content} keyboardShouldPersistTaps="handled">
            {activeTab === 'sql' && (
              <View>
                <Text style={styles.sectionLabel}>Enter SQL Statement</Text>
                <TextInput
                  style={styles.sqlInput}
                  value={sqlQuery}
                  onChangeText={setSqlQuery}
                  multiline
                  autoCapitalize="none"
                  autoCorrect={false}
                />

                <TouchableOpacity
                  style={styles.runButton}
                  onPress={handleRunSql}
                  disabled={isExecutingSql}
                >
                  {isExecutingSql ? (
                    <ActivityIndicator size="small" color="#FFFFFF" />
                  ) : (
                    <Text style={styles.runButtonText}>Execute Query (⚡ JSI)</Text>
                  )}
                </TouchableOpacity>

                {sqlError ? (
                  <View style={styles.errorBox}>
                    <Text style={styles.errorBoxText}>{sqlError}</Text>
                  </View>
                ) : null}

                {sqlResults && (
                  <View style={styles.resultsBox}>
                    <Text style={styles.resultsSummary}>
                      {sqlResults.rows.length} rows returned{' '}
                      {sqlResults.rowsAffected != null ? `(${sqlResults.rowsAffected} affected)` : ''}
                    </Text>

                    <ScrollView horizontal>
                      <View>
                        {/* Table Header */}
                        <View style={styles.tableRowHeader}>
                          {sqlResults.columns.map((col) => (
                            <Text key={col} style={styles.tableCellHeader}>
                              {col}
                            </Text>
                          ))}
                        </View>

                        {/* Table Rows */}
                        {sqlResults.rows.map((row, i) => (
                          <View key={i} style={[styles.tableRow, i % 2 === 1 && styles.tableRowAlt]}>
                            {sqlResults.columns.map((col) => (
                              <Text key={col} style={styles.tableCell}>
                                {String(row[col] ?? 'NULL')}
                              </Text>
                            ))}
                          </View>
                        ))}
                      </View>
                    </ScrollView>
                  </View>
                )}
              </View>
            )}

            {activeTab === 'storage' && (
              <View style={{ paddingTop: 8 }}>
                <LiliaStorageBar db={getDb()} />
              </View>
            )}

            {activeTab === 'kv' && (
              <View>
                <Text style={styles.sectionLabel}>Namespace</Text>
                <TextInput
                  style={styles.inputSingle}
                  value={kvNamespace}
                  onChangeText={setKvNamespace}
                  autoCapitalize="none"
                />

                <Text style={styles.sectionLabel}>Key</Text>
                <TextInput
                  style={styles.inputSingle}
                  value={kvKey}
                  onChangeText={setKvKey}
                  autoCapitalize="none"
                />

                <View style={styles.buttonRow}>
                  <TouchableOpacity style={styles.actionBtn} onPress={handleKvGet}>
                    <Text style={styles.actionBtnText}>Read Peek</Text>
                  </TouchableOpacity>
                  <TouchableOpacity style={[styles.actionBtn, styles.saveBtn]} onPress={handleKvSet}>
                    <Text style={[styles.actionBtnText, styles.saveBtnText]}>Save Value</Text>
                  </TouchableOpacity>
                </View>

                <Text style={styles.sectionLabel}>Value</Text>
                <TextInput
                  style={styles.sqlInput}
                  value={kvValue ?? ''}
                  onChangeText={setKvValue}
                  multiline
                  placeholder="<Value content>"
                />

                {kvStatus ? <Text style={styles.statusText}>{kvStatus}</Text> : null}
              </View>
            )}
          </ScrollView>
        </SafeAreaView>
      </Modal>
    </>
  );
}

const styles = StyleSheet.create({
  bubble: {
    position: 'absolute',
    backgroundColor: '#1E293B',
    paddingVertical: 8,
    paddingHorizontal: 14,
    borderRadius: 20,
    shadowColor: '#000',
    shadowOpacity: 0.3,
    shadowRadius: 6,
    elevation: 8,
    zIndex: 9999,
  },
  bubbleText: {
    color: '#38BDF8',
    fontSize: 13,
    fontWeight: '700',
  },
  modalContainer: {
    flex: 1,
    backgroundColor: '#F8FAFC',
  },
  modalHeader: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    alignItems: 'center',
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: 1,
    borderBottomColor: '#E2E8F0',
    backgroundColor: '#FFFFFF',
  },
  modalTitle: {
    fontSize: 18,
    fontWeight: '800',
    color: '#0F172A',
  },
  modalSubtitle: {
    fontSize: 12,
    color: '#64748B',
    maxWidth: 260,
  },
  closeButton: {
    padding: 8,
  },
  closeButtonText: {
    fontSize: 18,
    fontWeight: '700',
    color: '#64748B',
  },
  tabBar: {
    flexDirection: 'row',
    backgroundColor: '#FFFFFF',
    borderBottomWidth: 1,
    borderBottomColor: '#E2E8F0',
  },
  tab: {
    flex: 1,
    paddingVertical: 12,
    alignItems: 'center',
  },
  activeTab: {
    borderBottomWidth: 2,
    borderBottomColor: '#2563EB',
  },
  tabText: {
    fontSize: 14,
    fontWeight: '600',
    color: '#64748B',
  },
  activeTabText: {
    color: '#2563EB',
  },
  content: {
    flex: 1,
    padding: 16,
  },
  sectionLabel: {
    fontSize: 12,
    fontWeight: '700',
    color: '#475569',
    marginBottom: 6,
    marginTop: 10,
    textTransform: 'uppercase',
  },
  sqlInput: {
    backgroundColor: '#FFFFFF',
    borderColor: '#CBD5E1',
    borderWidth: 1,
    borderRadius: 8,
    padding: 12,
    fontSize: 14,
    fontFamily: 'monospace',
    minHeight: 80,
    textAlignVertical: 'top',
  },
  inputSingle: {
    backgroundColor: '#FFFFFF',
    borderColor: '#CBD5E1',
    borderWidth: 1,
    borderRadius: 8,
    padding: 10,
    fontSize: 14,
  },
  runButton: {
    backgroundColor: '#2563EB',
    paddingVertical: 12,
    borderRadius: 8,
    alignItems: 'center',
    marginTop: 10,
  },
  runButtonText: {
    color: '#FFFFFF',
    fontSize: 14,
    fontWeight: '700',
  },
  errorBox: {
    backgroundColor: '#FEF2F2',
    borderWidth: 1,
    borderColor: '#F87171',
    padding: 12,
    borderRadius: 8,
    marginTop: 12,
  },
  errorBoxText: {
    color: '#991B1B',
    fontSize: 13,
    fontFamily: 'monospace',
  },
  resultsBox: {
    marginTop: 16,
    backgroundColor: '#FFFFFF',
    borderRadius: 8,
    borderWidth: 1,
    borderColor: '#E2E8F0',
    padding: 12,
  },
  resultsSummary: {
    fontSize: 12,
    fontWeight: '600',
    color: '#64748B',
    marginBottom: 8,
  },
  tableRowHeader: {
    flexDirection: 'row',
    backgroundColor: '#F1F5F9',
    paddingVertical: 6,
    borderBottomWidth: 1,
    borderBottomColor: '#CBD5E1',
  },
  tableCellHeader: {
    minWidth: 100,
    paddingHorizontal: 8,
    fontSize: 12,
    fontWeight: '700',
    color: '#334155',
  },
  tableRow: {
    flexDirection: 'row',
    paddingVertical: 6,
    borderBottomWidth: 1,
    borderBottomColor: '#F1F5F9',
  },
  tableRowAlt: {
    backgroundColor: '#F8FAFC',
  },
  tableCell: {
    minWidth: 100,
    paddingHorizontal: 8,
    fontSize: 12,
    color: '#1E293B',
    fontFamily: 'monospace',
  },
  buttonRow: {
    flexDirection: 'row',
    marginVertical: 10,
  },
  actionBtn: {
    backgroundColor: '#E2E8F0',
    paddingVertical: 8,
    paddingHorizontal: 16,
    borderRadius: 6,
    marginRight: 8,
  },
  actionBtnText: {
    fontSize: 13,
    fontWeight: '600',
    color: '#334155',
  },
  saveBtn: {
    backgroundColor: '#10B981',
  },
  saveBtnText: {
    color: '#FFFFFF',
  },
  statusText: {
    marginTop: 10,
    fontSize: 13,
    color: '#059669',
    fontStyle: 'italic',
  },
});
