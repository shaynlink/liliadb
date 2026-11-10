const Module = require('node:module');
const originalLoad = Module._load;

const mockNativeModules = {};
const mockAppState = {
  addEventListener: () => ({ remove: () => {} }),
};

const mockFlatList = function MockFlatList(props) { return null; };
const mockSectionList = function MockSectionList(props) { return null; };
const mockRefreshControl = function MockRefreshControl(props) { return null; };
const mockView = function MockView(props) { return null; };
const mockText = function MockText(props) { return null; };
const mockTextInput = function MockTextInput(props) { return null; };
const mockTouchableOpacity = function MockTouchableOpacity(props) { return null; };
const mockSwitch = function MockSwitch(props) { return null; };
const mockModal = function MockModal(props) { return null; };
const mockScrollView = function MockScrollView(props) { return null; };
const mockSafeAreaView = function MockSafeAreaView(props) { return null; };
const mockActivityIndicator = function MockActivityIndicator(props) { return null; };
const mockStyleSheet = {
  create: (styles) => styles,
};
const mockShare = {
  share: async () => ({ action: 'sharedAction' }),
};

Module._load = function (request, parent, isMain) {
  if (request === 'react-native') {
    return {
      NativeModules: mockNativeModules,
      Platform: {
        OS: 'ios',
        select: (objs) => (objs && 'ios' in objs ? objs.ios : objs?.default),
      },
      AppState: mockAppState,
      FlatList: mockFlatList,
      SectionList: mockSectionList,
      RefreshControl: mockRefreshControl,
      View: mockView,
      Text: mockText,
      TextInput: mockTextInput,
      TouchableOpacity: mockTouchableOpacity,
      Switch: mockSwitch,
      Modal: mockModal,
      ScrollView: mockScrollView,
      SafeAreaView: mockSafeAreaView,
      ActivityIndicator: mockActivityIndicator,
      StyleSheet: mockStyleSheet,
      Share: mockShare,
    };
  }
  return originalLoad.apply(this, arguments);
};
