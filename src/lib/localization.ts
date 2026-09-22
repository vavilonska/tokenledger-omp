import type { AppSettings } from './types';

export type LanguagePreference = AppSettings['language'];

const zhHans: Record<string, string> = {
  'Partial credit estimate': 'credit 为部分估算',
  '5h Credits': '5 小时 credit',
  'Weekly Credits': '每周 credit',
  'Cost Display': '计价显示',
  'API USD': 'API 正价',
  'Codex credits': 'Codex credit',
  'Credit estimate unavailable': '暂无 credit 估算',
  'Share API USD Screenshot': '分享 API 正价截图',
  'Codex credits use a separate rate card. 2,500 credits = $100.':
    'Codex credit 独立计价；2,500 credit = 100 美元。',
  'Codex credit estimate · 2,500 credits = $100 · separate from API pricing':
    'Codex credit 估算 · 2,500 credit = 100 美元 · 独立于 API 计价',

  Settings: '设置',
  'Settings…': '设置…',
  Customize: '自定义',
  'Customize…': '自定义…',
  Options: '选项',
  'Options menu': '选项菜单',
  Back: '返回',
  Cancel: '取消',
  Close: '关闭',
  Done: '完成',
  Add: '添加',
  Edit: '编辑',
  Save: '保存',
  'Saving…': '正在保存…',
  Remove: '移除',
  'Removing…': '正在移除…',
  'Remove key': '移除密钥',
  Retry: '重试',
  'Retrying…': '正在重试…',
  Configure: '配置',
  Hide: '隐藏',
  'Show more': '显示更多',
  'Show less': '收起',
  Dismiss: '关闭提示',
  'Open options': '打开选项',
  'Share Screenshot': '分享截图',
  'Check for Updates…': '检查更新…',
  'About TokenLedger OMP': '关于 TokenLedger OMP',
  'Quit TokenLedger OMP': '退出 TokenLedger OMP',
  'Open TokenLedger OMP': '打开 TokenLedger OMP',
  'Hide TokenLedger OMP': '隐藏 TokenLedger OMP',
  'Close TokenLedger OMP': '关闭 TokenLedger OMP',
  'Return to Tray Popup': '返回托盘弹窗',
  'Keep Window Open': '保持窗口打开',
  'TokenLedger OMP usage dashboard': 'TokenLedger OMP 用量面板',
  'TokenLedger OMP window controls': 'TokenLedger OMP 窗口控件',
  'Resize panel height': '调整面板高度',
  'Drag to reorder. With a keyboard, use Alt plus Up Arrow or Alt plus Down Arrow.':
    '拖动可调整顺序。使用键盘时，请按 Alt 加上方向键上或方向键下。',
  'Refresh all provider usage': '刷新全部服务用量',
  'Reset all customization': '重置全部自定义项',
  'Reset All Customization': '重置全部自定义项',
  'Reset All Customization?': '重置全部自定义项？',
  'Reset All Settings?': '重置全部设置？',
  'Reset All': '全部重置',
  'Resetting…': '正在重置…',
  "This turns installed providers back on and restores every provider's metric visibility and order.":
    '这会重新启用已安装的服务，并恢复每个服务的指标可见性和排序。',
  'This restores appearance, notifications, shortcuts, updates, panel sizing, provider names, and layout. Provider sign-ins, API keys, and usage history stay in place. This cannot be undone.':
    '这会恢复外观、通知、快捷键、更新、面板尺寸、服务名称和布局。登录信息、API 密钥和用量历史不会被删除。此操作无法撤销。',
  'Private, local usage monitoring for your AI coding tools.':
    '私密、本地运行的 AI 编程工具用量监控器。',
  Version: '版本',
  'Loading TokenLedger OMP…': '正在加载 TokenLedger OMP…',
  'Waiting for first update': '等待首次更新',
  'Next update unavailable': '无法确定下次更新时间',
  'Updating…': '正在更新…',
  'TokenLedger OMP event bridge is unavailable.': 'TokenLedger OMP 事件桥不可用。',
  'TokenLedger OMP backend is unavailable.': 'TokenLedger OMP 后端不可用。',
  'Settings could not be reset.': '无法重置设置。',
  'Settings could not be saved.': '无法保存设置。',
  'Settings could not be saved or reloaded.': '无法保存或重新加载设置。',

  General: '通用',
  Language: '语言',
  'Follow System': '跟随系统',
  English: 'English',
  'Simplified Chinese': '简体中文',
  'Show Total Spend': '显示总花费',
  'Launch at Login': '登录时启动',
  'Global Shortcut': '全局快捷键',
  'Open TokenLedger OMP from anywhere': '在任意位置打开 TokenLedger OMP',
  'Type Shortcut…': '请按快捷键…',
  'Record Shortcut': '录制快捷键',
  'Clear global shortcut': '清除全局快捷键',
  'Activate to record. While recording, press a modifier shortcut to save it, Delete to clear it, or Escape to cancel.':
    '激活后开始录制。录制时按组合快捷键保存，按 Delete 清除，按 Esc 取消。',
  Appearance: '外观',
  'Icon Style': '图标样式',
  Text: '文字',
  Bars: '条形图',
  Theme: '主题',
  System: '系统',
  Light: '浅色',
  Dark: '深色',
  Density: '密度',
  Default: '默认',
  Compact: '紧凑',
  'Reduce Animations': '减少动画',
  'Window Mode': '窗口模式',
  'Tray Popup': '托盘弹窗',
  'Floating Window': '浮动窗口',
  'Panel Height': '面板高度',
  Automatic: '自动',
  Manual: '手动',
  'Time Format': '时间格式',
  Auto: '自动',
  '12-hour': '12 小时制',
  '24-hour': '24 小时制',
  'Usage Display': '用量显示',
  'Show Usage As': '用量显示方式',
  Left: '剩余',
  Used: '已用',
  'Reset Times': '重置时间',
  Countdown: '倒计时',
  'Exact Time': '准确时间',
  'Always Show Pacing': '始终显示用量节奏',
  "Show how you're pacing on every metric, not just ones near their limit":
    '在所有指标上显示用量节奏，而不仅是接近限额的指标',
  Notifications: '通知',
  'Almost Out': '即将用尽',
  'Cutting It Close': '接近用尽',
  'Will Run Out': '预计用尽',
  'Alert when a limit drops below 10% remaining.': '当剩余额度低于 10% 时提醒。',
  'Alert when a limit is projected to finish with little left.': '预计周期结束时剩余很少时提醒。',
  'Alert when a limit is projected to finish before it resets.': '预计在重置前用尽额度时提醒。',
  'Notifications are blocked': '通知已被阻止',
  'Permission is required': '需要通知权限',
  'Enable TokenLedger OMP notifications in system settings.':
    '请在系统设置中启用 TokenLedger OMP 通知。',
  'Allow notifications to receive the alerts selected above.': '允许通知后才能接收上方所选提醒。',
  'Open Settings': '打开设置',
  Allow: '允许',
  Advanced: '高级',
  'Log Level': '日志级别',
  Error: '错误',
  Warning: '警告',
  Info: '信息',
  Debug: '调试',
  'Copy Log Path': '复制日志路径',
  'Reveal in Finder': '在 Finder 中显示',
  'Reveal in File Explorer': '在文件资源管理器中显示',
  'Open Containing Folder': '打开所在文件夹',
  'Reset All Settings…': '重置全部设置…',
  Updates: '更新',
  'Check for Updates Automatically': '自动检查更新',
  'Checking…': '正在检查…',
  "Couldn't copy the log path to the clipboard.": '无法将日志路径复制到剪贴板。',
  "Couldn't reveal the log file.": '无法显示日志文件。',
  'Providers, metrics and menu bar': '服务、指标和菜单栏',
  'Notifications, appearance and more': '通知、外观及更多设置',
  "Choose what's visible and where": '选择显示的内容及位置',
  'Desktop Integration': '桌面集成',

  'Welcome to TokenLedger OMP': '欢迎使用 TokenLedger OMP',
  'We set you up with the AI tools found on your computer. Add or hide providers any time.':
    '已根据本机检测到的 AI 工具完成初始配置。你可以随时添加或隐藏服务。',
  'Open Customize': '打开自定义',
  'Turn on Customize to choose what to show.': '请在“自定义”中选择要显示的内容。',
  'Update Available': '有可用更新',
  'What’s new': '更新内容',
  'Update download': '更新下载',
  'Installing update…': '正在安装更新…',
  'Download interrupted. Retrying…': '下载中断，正在重试…',
  'Downloading update…': '正在下载更新…',
  'Try Again': '重试',
  'Install Update': '安装更新',
  'Download from GitHub': '从 GitHub 下载',
  'View Release': '查看版本',
  Outdated: '数据已过期',
  Refreshing: '正在刷新',
  'Last update time unavailable': '无法获取上次更新时间',
  'Last updated moments ago': '刚刚更新',
  Rename: '重命名',
  'Rename…': '重命名…',
  'Rename Card': '重命名卡片',
  Name: '名称',
  'Leave the name empty to go back to the default.': '名称留空可恢复默认值。',
  'Star for menu bar': '固定到菜单栏',
  Unstar: '从菜单栏取消固定',
  'Starred for menu bar': '已固定到菜单栏',
  'Removed from menu bar': '已从菜单栏移除',
  'Up to 2 stars per provider': '每个服务最多固定 2 项',
  'Always Visible': '始终显示',
  'On Demand': '按需显示',
  'Always Visible metrics': '始终显示的指标',
  'On Demand metrics': '按需显示的指标',
  'Drag metrics here': '将指标拖到这里',
  'Detected on this computer': '已在此计算机上检测到',
  'Not detected on this computer': '未在此计算机上检测到',

  'Total Spend': '总花费',
  'Total Spend Metric': '总花费指标',
  'Total Spend period': '总花费周期',
  Cost: '费用',
  'Cost/MTok': '每百万 token 费用',
  Tokens: 'token',
  Today: '今天',
  Yesterday: '昨天',
  '30 Days': '30 天',
  dollars: '美元',
  billion: '十亿',
  million: '百万',
  thousand: '千',
  tokens: 'token',
  'No token usage yet': '暂无 token 用量',
  'No priced usage yet': '暂无已计价用量',
  'Estimated locally, so it may be off': '根据本地数据估算，可能存在偏差',
  'Estimated locally, so it may differ from billed usage.':
    '根据本地数据估算，可能与账单用量不同。',
  'Estimated value': '估算值',
  'No data': '暂无数据',
  'Usage Trend': '用量趋势',
  'Usage trend chart details': '用量趋势图详情',
  Session: '会话',
  Weekly: '每周',
  'Spark Weekly': 'Spark 每周',
  "Today's Usage": '今日用量',
  "Yesterday's Usage": '昨日用量',
  'Last 30 Days': '最近 30 天',
  'Session API Value': '会话 API 正价',
  '5h API Value': '5 小时 API 正价',
  'Weekly API Value': '每周 API 正价',
  'Extra Usage': '额外用量',
  'Rate Limit Resets': '限额重置次数',
  Credits: '余额',
  credits: '余额',
  available: '可用',
  spent: '已花费',
  'Org Credits': '组织余额',
  'Org Spend': '组织花费',
  'Extra Usage Balance': '额外用量余额',
  Requests: '请求数',
  'Web Searches': '网页搜索',
  'Pay as you go': '按量付费',
  Status: '状态',
  Dashboard: '控制台',
  Activity: '活动',
  'API Keys': 'API 密钥',
  Usage: '用量',
  'From your Codex logs (estimated)': '来自 Codex 日志（估算）',
  'From your Claude usage history (estimated)': '来自 Claude 用量历史（估算）',
  'From your Cursor usage export': '来自 Cursor 用量导出',
  'From your Grok logs (estimated)': '来自 Grok 日志（估算）',
  'Actual reset cycles': '实际重置周期',
  Current: '当前',
  used: '已用',
  quota: '额度',
  'quota unavailable': '额度不可用',
  observed: '已观测',
  'Server-observed windows; reset cause is not inferred.': '周期来自服务器观测；不会推断重置原因。',
  'Unknown model found': '发现未知模型',
  'Unknown models found': '发现未知模型',
  'Estimated quota': '估算额度',
  'Will reach limit': '预计达到限额',
  'Sessions start after you send your first message.': '发送第一条消息后才会开始会话周期。',
  'Not started': '尚未开始',
  'This period used a model with unknown pricing': '此周期使用了价格未知的模型',
  'Estimated from local usage data and may differ from billed usage.':
    '根据本地用量数据估算，可能与账单用量不同。',
  'Limit reached': '已达到限额',
  'Reset unavailable': '无法获取重置时间',
  'Expiry times unavailable': '无法获取到期时间',
  'Resets expire in:': '重置卡将在以下时间后到期：',
  'Resets expire:': '重置卡到期时间：',
  'Expiring soon': '即将到期',
  'Reset applied.': '已执行重置。',
  'No active limit needs resetting.': '当前没有需要重置的限额。',
  'This reset is no longer available.': '此重置卡已不可用。',
  'Could not use this reset. Try again.': '无法使用此重置卡，请重试。',
  'Use this reset?': '使用此重置卡？',
  "Immediately reset your usage limits. This can't be undone.":
    '立即重置用量限额。此操作无法撤销。',
  'Use reset': '使用重置卡',
  Use: '使用',
  'No rate limit resets available': '没有可用的限额重置卡',

  'API Key': 'API 密钥',
  'From Your Environment': '来自环境变量',
  'From Config File': '来自配置文件',
  'Saved securely': '已安全保存',
  'Custom Key': '自定义密钥',
  'Paste API key': '粘贴 API 密钥',
  'Hide API key': '隐藏 API 密钥',
  'Show API key': '显示 API 密钥',
  'Remove saved API key': '移除已保存的 API 密钥',
  'Remove saved API key?': '移除已保存的 API 密钥？',
  "The saved key will be removed from secure storage. This can't be undone.":
    '已保存的密钥将从安全存储中移除。此操作无法撤销。',
  'Override With a Custom Key': '使用自定义密钥覆盖',
  'The API key could not be saved.': '无法保存 API 密钥。',
  'The saved API key could not be removed.': '无法移除已保存的 API 密钥。',
  'The system credential store is unavailable.': '系统凭据存储不可用。',
};

let preference: LanguagePreference = 'system';
let detectedSystemLanguage: 'en' | 'zh-CN' =
  typeof navigator !== 'undefined' && /^zh(?:-|$)/i.test(navigator.language) ? 'zh-CN' : 'en';
let observer: MutationObserver | null = null;
const originalText = new WeakMap<Text, string>();
const originalAttributes = new WeakMap<Element, Map<string, string>>();
const translatedAttributes = ['aria-label', 'data-tooltip', 'title', 'placeholder'] as const;

export function resolvedLanguage(value: LanguagePreference = preference): 'en' | 'zh-CN' {
  if (value === 'english') return 'en';
  if (value === 'simplifiedChinese') return 'zh-CN';
  return detectedSystemLanguage;
}

export function translate(source: string, language = resolvedLanguage()): string {
  if (language !== 'zh-CN' || !source) return source;
  const match = source.match(/^(\s*)([\s\S]*?)(\s*)$/);
  if (!match) return source;
  const [, leading, value, trailing] = match;
  return `${leading}${translateValue(value)}${trailing}`;
}

function translateValue(value: string): string {
  const exact = zhHans[value];
  if (exact) return exact;
  const normalized = value.replace(/\s+/g, ' ').trim();
  const normalizedExact = zhHans[normalized];
  if (normalizedExact) return normalizedExact;
  if (value.includes(' · ')) return value.split(' · ').map(translateValue).join(' · ');

  const replacements: Array<[RegExp, (...parts: string[]) => string]> = [
    [/^Next update in (\d+)m$/, (amount) => `${amount} 分钟后更新`],
    [/^Next update in (\d+)s$/, (amount) => `${amount} 秒后更新`],
    [/^Last updated (\d+)m ago$/, (amount) => `${amount} 分钟前更新`],
    [
      /^Last updated (\d+)h(?: (\d+)m)? ago$/,
      (hours, minutes) => `${hours} 小时${minutes ? ` ${minutes} 分钟` : ''}前更新`,
    ],
    [
      /^TokenLedger OMP (.+) is ready to download\.$/,
      (version) => `TokenLedger OMP ${version} 已可下载。`,
    ],
    [
      /^TokenLedger OMP (.+) is up to date\.$/,
      (version) => `TokenLedger OMP ${version} 已是最新版本。`,
    ],
    [/^TokenLedger OMP (.+) is available\.$/, (version) => `TokenLedger OMP ${version} 已发布。`],
    [/^Downloading update… (\d+)%$/, (percent) => `正在下载更新… ${percent}%`],
    [/^Refresh (.+)$/, (name) => `刷新 ${name}`],
    [/^Hide (.+)$/, (name) => `隐藏 ${name}`],
    [/^Move (.+)$/, (name) => `移动 ${translateValue(name)}`],
    [/^Enable (.+)$/, (name) => `启用 ${name}`],
    [/^Customize (.+)$/, (name) => `自定义 ${name}`],
    [/^Pin (.+)$/, (name) => `固定 ${translateValue(name)}`],
    [/^Unpin (.+)$/, (name) => `取消固定 ${translateValue(name)}`],
    [/^Show (.+)$/, (name) => `显示 ${translateValue(name)}`],
    [/^Drag (.+) to reorder$/, (name) => `拖动 ${name} 进行排序`],
    [/^Configure (.+)$/, (name) => `配置 ${name}`],
    [/^Retrying (.+)$/, (name) => `正在重试 ${name}`],
    [/^Retry (.+)$/, (name) => `重试 ${name}`],
    [/^Reset (.+)$/, (name) => `重置 ${name}`],
    [/^Name for (.+)$/, (name) => `${name} 的名称`],
    [/^(.+) provider$/, (name) => `${name} 服务`],
    [/^(.+) usage$/, (name) => `${name} 用量`],
    [/^(\d+) metrics$/, (amount) => `${amount} 个指标`],
    [/^(.+) options$/, (name) => `${translateValue(name)}选项`],
    [/^(.+) model usage$/, (name) => `${translateValue(name)}模型用量`],
    [/^(.+) reset cycle history$/, (name) => `${translateValue(name)}重置周期历史`],
    [/^(.+) details$/, (name) => `${translateValue(name)}详情`],
    [/^(.+), opens in browser$/, (name) => `${translateValue(name)}，在浏览器中打开`],
    [/^Only includes (.+?)(?:\.)?$/, (names) => `仅包含 ${names.replaceAll(' and ', '、')}。`],
    [/^Share (.+) Screenshot$/, (name) => `分享${translateValue(name)}截图`],
    [/^Use reset expiring (.+)$/, (time) => `使用将于 ${time} 到期的重置卡`],
    [/^(\d+) available$/, (amount) => `${amount} 个可用`],
    [/^(.+): (\d+) available$/, (name, amount) => `${translateValue(name)}：${amount} 个可用`],
    [/^~(.+) quota$/, (amount) => `额度约 ${amount}`],
    [/^(.+) quota$/, (name) => `${translateValue(name)}额度`],
    [/^(.+) used$/, (amount) => `已用 ${translateValue(amount)}`],
    [/^(.+)% observed$/, (amount) => `已观测 ${amount}%`],
    [/^(.+) tokens$/, (amount) => `${amount} token`],
    [/^(.+) credits$/, (amount) => `${amount} 余额`],
    [
      /^(.+) requests (left|used)$/,
      (amount, state) => `${amount} 次请求${state === 'left' ? '剩余' : '已用'}`,
    ],
    [/^(.+) (left|spent)$/, (amount, state) => `${state === 'left' ? '剩余' : '已花费'} ${amount}`],
    [/^(.+)% (left|used)$/, (amount, state) => `${state === 'left' ? '剩余' : '已用'} ${amount}%`],
    [/^~(.+)% spare$/, (amount) => `预计剩余约 ${amount}%`],
    [/^Estimated quota: (.+)$/, (amount) => `估算额度：${amount}`],
    [
      /^Inferred from (.+)% used; model mix affects this estimate$/,
      (amount) => `根据已用 ${amount}% 推算；模型组合会影响估算结果`,
    ],
    [/^peak (.+) tokens$/, (amount) => `峰值 ${amount} token`],
    [
      /^30-day token chart\. Peak (.+) tokens on (.+)\.$/,
      (amount, date) => `30 天 token 图。峰值 ${amount}，日期 ${date}。`,
    ],
    [/^Resets in (.+)$/, (duration) => `${duration} 后重置`],
    [/^Resets soon$/, () => '即将重置'],
    [/^Resets today at (.+)$/, (time) => `今天 ${time} 重置`],
    [/^Resets tomorrow at (.+)$/, (time) => `明天 ${time} 重置`],
    [/^Resets (.+) at (.+)$/, (day, time) => `${day} ${time} 重置`],
    [/^Limit in (.+)$/, (duration) => `${duration} 后达到限额`],
    [/^Limit soon$/, () => '即将达到限额'],
    [/^~(.+)% left at reset$/, (amount) => `重置时预计剩余约 ${amount}%`],
    [/^~(.+)% used at reset$/, (amount) => `重置时预计已用约 ${amount}%`],
    [/^~(.+)% over limit at reset$/, (amount) => `重置时预计超出限额约 ${amount}%`],
  ];

  for (const [pattern, replacement] of replacements) {
    const match = value.match(pattern);
    if (match) return replacement(...match.slice(1));
  }
  return value;
}

export function setLanguagePreference(value: LanguagePreference) {
  preference = value;
  if (typeof document === 'undefined') return;
  document.documentElement.lang = resolvedLanguage(value);
  applyTree(document.documentElement);
}

export function setSystemLanguage(value: 'en' | 'zh-CN') {
  detectedSystemLanguage = value;
  if (preference === 'system') setLanguagePreference(preference);
}

export function installLocalization() {
  if (typeof document === 'undefined' || observer) return () => undefined;
  document.documentElement.lang = resolvedLanguage();
  applyTree(document.documentElement);
  observer = new MutationObserver((mutations) => {
    for (const mutation of mutations) {
      if (mutation.type === 'characterData') applyText(mutation.target as Text);
      if (mutation.type === 'attributes')
        applyAttribute(mutation.target as Element, mutation.attributeName!);
      for (const node of mutation.addedNodes) applyTree(node);
    }
  });
  observer.observe(document.documentElement, {
    subtree: true,
    childList: true,
    characterData: true,
    attributes: true,
    attributeFilter: [...translatedAttributes],
  });
  return () => {
    observer?.disconnect();
    observer = null;
  };
}

function applyTree(root: Node) {
  if (root.nodeType === Node.TEXT_NODE) applyText(root as Text);
  if (root instanceof Element) {
    for (const attribute of translatedAttributes) applyAttribute(root, attribute);
  }
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT | NodeFilter.SHOW_TEXT);
  let node = walker.nextNode();
  while (node) {
    if (node.nodeType === Node.TEXT_NODE) applyText(node as Text);
    else if (node instanceof Element) {
      for (const attribute of translatedAttributes) applyAttribute(node, attribute);
    }
    node = walker.nextNode();
  }
}

function applyText(node: Text) {
  const current = node.data;
  let source = originalText.get(node);
  if (source === undefined || (current !== source && current !== translate(source, 'zh-CN'))) {
    source = current;
    originalText.set(node, source);
  }
  const next = translate(source);
  if (next !== current) node.data = next;
}

function applyAttribute(element: Element, attribute: string) {
  const current = element.getAttribute(attribute);
  if (current === null) return;
  let sources = originalAttributes.get(element);
  if (!sources) {
    sources = new Map();
    originalAttributes.set(element, sources);
  }
  let source = sources.get(attribute);
  if (source === undefined || (current !== source && current !== translate(source, 'zh-CN'))) {
    source = current;
    sources.set(attribute, source);
  }
  const next = translate(source);
  if (next !== current) element.setAttribute(attribute, next);
}
