'use strict';

const tip = document.getElementById('tip');

async function activeTab() {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  return tab;
}

async function refreshTip() {
  const tab = await activeTab();
  if (tab && tab.url && tab.url.includes('log.wwitil.woa.com')) {
    tip.innerHTML = '已检测到日志查询页，可直接抓取参数。<span class="ok">（建议使用已执行查询、URL 含 page=query 的链接）</span>';
  } else {
    tip.textContent = '当前标签页不是 log.wwitil.woa.com，请先打开日志查询页，或直接打开收集器手动粘贴 URL。';
  }
}

document.getElementById('btnGrab').addEventListener('click', async () => {
  const tab = await activeTab();
  if (!tab || !tab.url || !tab.url.includes('log.wwitil.woa.com')) {
    tip.textContent = '未检测到日志查询页，请先打开并执行一次查询。';
    return;
  }
  await chrome.storage.local.set({ tplUrl: tab.url });
  chrome.tabs.create({ url: chrome.runtime.getURL('collector.html') });
});

document.getElementById('btnOpen').addEventListener('click', () => {
  chrome.tabs.create({ url: chrome.runtime.getURL('collector.html') });
});

refreshTip();
