import 'dart:convert';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:liliadb_flutter/liliadb_flutter.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  group('LiliadbOptions and Profiles', () {
    test('default constructor sets mobile profile and autoFlush true', () {
      const options = LiliadbOptions(
        path: 'test.db',
        profile: LiliadbProfile.mobile,
        autoFlushOnBackground: true,
      );

      expect(options.path, equals('test.db'));
      expect(options.profile, equals(LiliadbProfile.mobile));
      expect(options.autoFlushOnBackground, isTrue);
      expect(options.useSecureStorage, isFalse);
    });

    test('constrained profile configuration for watchOS / wearables', () {
      const options = LiliadbOptions(
        path: 'watch.db',
        profile: LiliadbProfile.constrained,
        useSecureStorage: true,
      );

      expect(options.path, equals('watch.db'));
      expect(options.profile, equals(LiliadbProfile.constrained));
      expect(options.useSecureStorage, isTrue);
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
    test('can open and execute KV operations', () async {
      final db = await LiliaDatabase.open(path: ':memory:');
      expect(db.isClosed, isFalse);

      // Synchronous peek before any write
      expect(db.sync.kv.peek('prefs', 'lang'), isNull);

      // Synchronous set
      db.sync.kv.set('prefs', 'lang', 'fr');
      expect(db.sync.kv.peek('prefs', 'lang'), equals('fr'));

      final entry = db.sync.kv.get('prefs', 'lang');
      expect(entry, isNotNull);
      expect(entry!.valueString, equals('fr'));

      // Asynchronous API
      await db.kv.set('prefs', 'theme', 'dark');
      final asyncEntry = await db.kv.get('prefs', 'theme');
      expect(asyncEntry?.valueString, equals('dark'));

      // Delete
      expect(db.sync.kv.delete('prefs', 'lang'), isTrue);
      expect(db.sync.kv.peek('prefs', 'lang'), isNull);

      await db.close();
      expect(db.isClosed, isTrue);
    });

    test('can execute JSON documents and cursor find', () async {
      final db = await LiliaDatabase.open(path: ':memory:');

      // Put document
      db.sync.json.put('users', 'u1', {'name': 'Ada Lovelace', 'age': 36});
      final doc = db.sync.json.get<Map<String, dynamic>>('users', 'u1');
      expect(doc, isNotNull);
      expect(doc!.value['name'], equals('Ada Lovelace'));

      // Find documents
      final list = db.sync.json.find<Map<String, dynamic>>('users', {});
      expect(list, isA<List<JsonEntry<Map<String, dynamic>>>>());

      // Async put
      await db.json.put('users', 'u2', {'name': 'Alan Turing'});
      final u2 = await db.json.get<Map<String, dynamic>>('users', 'u2');
      expect(u2?.value['name'], equals('Alan Turing'));

      await db.close();
    });

    test('can execute Graph engine operations', () async {
      final db = await LiliaDatabase.open(path: ':memory:');

      db.sync.graph.setNode('social', 'alice', 'user', {'role': 'admin'});
      db.sync.graph.setNode('social', 'bob', 'user', {'role': 'guest'});
      db.sync.graph.addEdge('social', 'alice', 'bob', 'follows');

      final neighbors = db.sync.graph.neighbors('social', 'alice');
      expect(neighbors, isA<List<String>>());

      await db.close();
    });

    test('can execute FTS5 and SQL queries', () async {
      final db = await LiliaDatabase.open(path: ':memory:');

      db.sync.fts.index('docs', 'd1', 'LiliaDB is lightning fast');
      final hits = db.sync.fts.search('docs', 'lightning');
      expect(hits, isA<List<FtsHit>>());

      final sqlRes = db.sync.sql.query('SELECT 1 as num');
      expect(sqlRes, isA<SqlQueryResult>());
      expect(sqlRes.columns, isNotNull);

      await db.close();
    });

    test('can execute storage maintenance', () async {
      final db = await LiliaDatabase.open(path: ':memory:');

      final metrics = db.sync.maintenance.getFileSize();
      expect(metrics.totalBytes, isNotNull);

      db.sync.maintenance.checkpoint();
      db.sync.maintenance.vacuum();

      await db.close();
    });
  });

  group('20 Built-in Widgets Instantiation', () {
    testWidgets('all 20 widgets can be instantiated and constructed', (tester) async {
      final db = await LiliaDatabase.open(path: ':memory:');

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
        LiliaBackupShareButton(
          database: db,
        ),
        LiliaDevInspector(
          enabled: false,
          database: db,
        ),
      ];

      expect(widgets.length, equals(20));
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
