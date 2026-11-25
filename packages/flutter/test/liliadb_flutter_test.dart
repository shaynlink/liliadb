import 'dart:io';
import 'package:material_ui/material_ui.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:liliadb_flutter/liliadb_flutter.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  /// A database in a new temporary directory; the engine library comes from `LILIA_FFI_PATH`.
  Future<LiliaDatabase> openTemporary() {
    final directory = Directory.systemTemp.createTempSync('lilia-flutter-');
    addTearDown(() => directory.deleteSync(recursive: true));
    return LiliaDatabase.open(path: directory.path, durability: Durability.performance);
  }

  group('LiliadbOptions and Profiles', () {
    test('defaults: mobile profile, durable, no ledger, autoFlush', () {
      const options = LiliadbOptions(path: 'data');
      expect(options.profile, equals(LiliadbProfile.mobile));
      expect(options.durability, equals(Durability.durable));
      expect(options.autoFlushOnBackground, isTrue);
      expect(options.toJson(), {
        'path': 'data',
        'profile': 'mobile',
        'durability': 'durable',
        'ledger': false,
        'undo_depth': 0,
      });
    });

    test('constrained profile with a history window and undo', () {
      const options = LiliadbOptions(
        path: 'watch',
        profile: LiliadbProfile.constrained,
        historyRetentionMs: 60000,
        undoDepth: 8,
      );
      expect(options.toJson()['profile'], equals('constrained'));
      expect(options.toJson()['history_retention_ms'], equals(60000));
      expect(options.toJson()['undo_depth'], equals(8));
    });
  });

  group('DatabaseObserver reactive event matching', () {
    test('notifies subscribers matching exact and wildcard patterns', () {
      final observer = DatabaseObserver();
      int kvCount = 0;
      int anyKvCount = 0;
      int tableCount = 0;

      final unsub1 = observer.subscribe('kv:settings:theme', () => kvCount++);
      final unsub2 = observer.subscribe('kv:settings', () => anyKvCount++);
      final unsub3 = observer.subscribe('table:users', () => tableCount++);

      observer.emit(const DatabaseKvChangeEvent(
        namespace: 'settings',
        key: 'theme',
        op: 'set',
      ));

      expect(kvCount, equals(1));
      expect(anyKvCount, equals(1));
      expect(tableCount, equals(0));

      // Emit table change
      observer.emit(const DatabaseTableChangeEvent(table: 'users'));
      expect(tableCount, equals(1));

      // Unsubscribe works
      unsub1();
      observer.emit(const DatabaseKvChangeEvent(
        namespace: 'settings',
        key: 'theme',
        op: 'set',
      ));
      expect(kvCount, equals(1)); // Has not incremented
      expect(anyKvCount, equals(2));

      unsub2();
      unsub3();
    });
  });

  group('LiliaDatabase synchronous and asynchronous engine', () {
    test('KV: peek, get, set, scan, delete', () async {
      final db = await openTemporary();
      expect(db.sync.kv.peek('prefs', 'lang'), isNull);
      final version = db.sync.kv.set('prefs', 'lang', 'fr');
      expect(db.sync.kv.peek('prefs', 'lang'), equals('fr'));
      final entry = db.sync.kv.get('prefs', 'lang')!;
      expect(entry.valueString, equals('fr'));
      expect(entry.version, equals(version));

      await db.kv.set('prefs', 'theme', 'dark');
      expect((await db.kv.get('prefs', 'theme'))?.valueString, equals('dark'));
      expect(db.sync.kv.scan('prefs', limit: 0).length, equals(2));

      expect(db.sync.kv.delete('prefs', 'lang'), isTrue);
      expect(db.sync.kv.peek('prefs', 'lang'), isNull);
      await db.close();
      expect(db.isClosed, isTrue);
      expect(() => db.sync.kv.get('prefs', 'theme'), throwsA(isA<LiliaException>()));
    });

    test('JSON: documents, indexes and queries', () async {
      final db = await openTemporary();
      db.sync.json.put('users', 'u1', {'name': 'Ada Lovelace', 'role': 'admin'});
      await db.json.put('users', 'u2', {'name': 'Alan Turing', 'role': 'guest'});
      expect(db.sync.json.get<Map<String, dynamic>>('users', 'u1')!.value['name'], equals('Ada Lovelace'));
      expect(db.sync.json.scan<Map<String, dynamic>>('users').map((d) => d.id), equals(['u1', 'u2']));

      db.sync.json.createIndex('users', 'role', r'$.role');
      final admins = db.sync.json.find<Map<String, dynamic>>('users', {'role': 'admin'});
      expect(admins.map((d) => d.id), equals(['u1']));
      expect(db.sync.json.listIndexes('users').single.indexName, equals('role'));
      expect(db.sync.json.dropIndex('users', 'role'), isTrue);
      await db.close();
    });

    test('Graph: nodes, edges, neighbours, reachability', () async {
      final db = await openTemporary();
      db.sync.graph.setNode('social', 'alice', 'user', {'role': 'admin'});
      db.sync.graph.setNode('social', 'bob', 'user', {'role': 'guest'});
      db.sync.graph.addEdge('social', 'alice', 'bob', 'follows');
      expect(db.sync.graph.neighbors('social', 'alice'), equals(['bob']));
      expect(db.sync.graph.reachability('social', ['alice']), containsAll(['alice', 'bob']));
      expect(db.sync.graph.getNode<Map<String, dynamic>>('social', 'alice')!.metadata['role'], equals('admin'));
      await db.close();
    });

    test('Full-text search and SQL', () async {
      final db = await openTemporary();
      db.sync.fts.index('docs', 'd1', 'LiliaDB is lightning fast');
      expect(db.sync.fts.search('docs', 'lightning').single.docId, equals('d1'));

      db.sync.json.put('users', 'u1', {'name': 'Ada'});
      final rows = db.sync.sql.query<Map<String, dynamic>>(
        "SELECT id, value->>'name' AS name FROM users WHERE id = \$1",
        ['u1'],
      );
      expect(rows.columns, equals(['id', 'name']));
      expect(rows.rows.single, equals({'id': 'u1', 'name': 'Ada'}));
      await db.close();
    });

    test('Maintenance: checkpoint, checks, storage statistics', () async {
      final db = await openTemporary();
      db.sync.kv.set('k', 'v', 'x');
      db.sync.maintenance.checkpoint();
      expect(db.sync.maintenance.integrityCheck(), isTrue);
      expect(db.sync.maintenance.quickCheck(), isTrue);
      final stats = db.sync.maintenance.storageStats();
      expect(stats.diskBytes, greaterThan(0));
      expect(stats.version, greaterThanOrEqualTo(stats.oldestVersion));
      await db.close();
    });
  });

  group('Built-in Widgets Instantiation', () {
    testWidgets('every widget can be instantiated', (tester) async {
      final db = await openTemporary();

      final widgets = [
        LiliaListView<Map<String, dynamic>>(
          sql: 'SELECT 1 as id',
          database: db,
          itemBuilder: (context, item, index) => Text('$item'),
        ),
        LiliaSectionListView<Map<String, dynamic>>(
          sql: 'SELECT 1 as id',
          database: db,
          groupBy: (item) => 'General',
          sectionHeaderBuilder: (context, title) => Text(title),
          itemBuilder: (context, item, index) => Text('$item'),
        ),
        LiliaInfiniteListView<Map<String, dynamic>>(
          space: 'users',
          database: db,
          itemBuilder: (context, item, index) => Text(item.id),
        ),
        LiliaKvSwitch(
          namespace: 'settings',
          keyName: 'dark_mode',
          database: db,
        ),
        LiliaKvTextField(
          namespace: 'drafts',
          keyName: 'note',
          database: db,
        ),
        LiliaDocumentView<Map<String, dynamic>>(
          space: 'settings',
          id: 'app',
          database: db,
          builder: (context, doc, update, delete) => const SizedBox(),
        ),
        LiliaForm<Map<String, dynamic>>(
          space: 'users',
          id: 'me',
          initialValues: const {'name': 'Test'},
          database: db,
          builder: (context, form) => const SizedBox(),
        ),
        LiliaCountBadge(
          count: 5,
          database: db,
        ),
        const LiliaErrorBoundary(
          child: SizedBox(),
        ),
        const LiliaSuspense(
          fallback: Text('Loading'),
          isReady: true,
          child: Text('Content'),
        ),
        LiliaOptimisticButton(
          onPressed: () async {},
          child: const Text('Submit'),
        ),
        LiliaSearchBar(
          collection: 'articles',
          database: db,
          onSearch: (hits) {},
        ),
        const LiliaHighlightSnippet(
          snippet: 'Found in <b>LiliaDB</b> test',
        ),
        LiliaGraphExplorer(
          graph: 'social',
          rootNode: 'alice',
          database: db,
        ),
        const LiliaGraphBreadcrumbs(
          path: ['Root', 'Child'],
        ),
        const LiliaBiometricLock(
          child: Text('Secret'),
        ),
        const LiliaSecureField(
          value: 'secret-key-123',
        ),
        LiliaStorageBar(
          database: db,
        ),
        LiliaDevInspector(
          enabled: false,
          database: db,
        ),
      ];

      expect(widgets.length, equals(19));
      for (final widget in widgets) {
        expect(widget, isA<Widget>());
      }

      await db.close();
    });

    testWidgets('LiliaSuspense renders fallback when loading and child when ready', (tester) async {
      await tester.pumpWidget(
        const MaterialApp(
          home: LiliaSuspense(
            isLoading: true,
            fallback: Text('Fallback Skeleton'),
            child: Text('Hydrated Content'),
          ),
        ),
      );

      expect(find.text('Fallback Skeleton'), findsOneWidget);
      expect(find.text('Hydrated Content'), findsNothing);

      await tester.pumpWidget(
        const MaterialApp(
          home: LiliaSuspense(
            isLoading: false,
            isReady: true,
            fallback: Text('Fallback Skeleton'),
            child: Text('Hydrated Content'),
          ),
        ),
      );

      expect(find.text('Hydrated Content'), findsOneWidget);
      expect(find.text('Fallback Skeleton'), findsNothing);
    });

    testWidgets('LiliaHighlightSnippet parses <b> tags into TextSpans', (tester) async {
      await tester.pumpWidget(
        const MaterialApp(
          home: Scaffold(
            body: LiliaHighlightSnippet(
              snippet: 'LiliaDB is <b>ultra fast</b> for Flutter',
            ),
          ),
        ),
      );

      final richTextFinder = find.byType(RichText);
      expect(richTextFinder, findsOneWidget);
    });

    testWidgets('LiliaCountBadge formats maxDisplay properly', (tester) async {
      await tester.pumpWidget(
        const MaterialApp(
          home: Scaffold(
            body: Column(
              children: [
                LiliaCountBadge(count: 42),
                LiliaCountBadge(count: 150, maxDisplay: 99),
                LiliaCountBadge(count: 0, hideZero: true),
              ],
            ),
          ),
        ),
      );

      expect(find.text('42'), findsOneWidget);
      expect(find.text('99+'), findsOneWidget);
      // Zero should be hidden
      expect(find.text('0'), findsNothing);
    });
  });
}
