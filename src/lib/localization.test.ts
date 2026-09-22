import { afterEach, describe, expect, it } from 'vitest';
import {
  installLocalization,
  resolvedLanguage,
  setLanguagePreference,
  setSystemLanguage,
  translate,
} from './localization';

describe('interface localization', () => {
  afterEach(() => {
    setSystemLanguage('en');
    setLanguagePreference('english');
    document.body.innerHTML = '';
  });

  it('translates fixed and dynamic usage copy into Simplified Chinese', () => {
    expect(translate('Settings', 'zh-CN')).toBe('设置');
    expect(translate('Weekly API Value', 'zh-CN')).toBe('每周 API 正价');
    expect(translate('Next update in 4m', 'zh-CN')).toBe('4 分钟后更新');
    expect(translate('Refresh Codex', 'zh-CN')).toBe('刷新 Codex');
    expect(translate('Tokens', 'zh-CN')).toBe('token');
    expect(translate('1.3B tokens', 'zh-CN')).toBe('1.3B token');
    expect(translate('$3.20 · Estimated locally, so it may be off', 'zh-CN')).toBe(
      '$3.20 · 根据本地数据估算，可能存在偏差',
    );
  });

  it('updates existing and newly rendered content and can switch back to English', async () => {
    setLanguagePreference('simplifiedChinese');
    document.body.innerHTML = '<button aria-label="Settings">Settings</button>';
    const stop = installLocalization();

    expect(document.querySelector('button')).toHaveTextContent('设置');
    expect(document.querySelector('button')).toHaveAttribute('aria-label', '设置');

    const row = document.createElement('p');
    row.textContent = 'No data';
    document.body.append(row);
    await Promise.resolve();
    expect(row).toHaveTextContent('暂无数据');

    setLanguagePreference('english');
    expect(document.querySelector('button')).toHaveTextContent('Settings');
    expect(row).toHaveTextContent('No data');
    stop();
  });

  it('uses the selected preference before falling back to the system locale', () => {
    expect(resolvedLanguage('english')).toBe('en');
    expect(resolvedLanguage('simplifiedChinese')).toBe('zh-CN');
    setSystemLanguage('zh-CN');
    expect(resolvedLanguage('system')).toBe('zh-CN');
  });
});
