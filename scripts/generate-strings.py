# The single source of truth for every user-facing string in the interface.
#
# Generates both sides of the boundary — the Slint global and the Rust catalogue — so a
# key can never exist on one side and not the other.
#
# Each entry: (key, english, russian)

ENTRIES = [
    # --- navigation ---------------------------------------------------------------
    ("app_title", "VDS Admin", "VDS Admin"),
    ("app_subtitle", "Infrastructure & analytics", "Инфраструктура и аналитика"),
    ("nav_dashboard", "Dashboard", "Сводка"),
    ("nav_home", "Home", "Сводка"),
    ("nav_servers", "Servers", "Серверы"),
    ("nav_websites", "Websites", "Сайты"),
    ("nav_sites_short", "Sites", "Сайты"),
    ("nav_analytics", "Analytics", "Аналитика"),
    ("nav_alerts", "Alerts", "Оповещения"),
    ("nav_settings", "Settings", "Настройки"),
    ("nav_more", "More", "Ещё"),

    # --- common actions -----------------------------------------------------------
    ("action_add", "Add", "Добавить"),
    ("action_cancel", "Cancel", "Отмена"),
    ("action_save", "Save", "Сохранить"),
    ("action_remove", "Remove", "Удалить"),
    ("action_refresh", "Refresh", "Обновить"),
    ("action_retry", "Retry", "Повторить"),
    ("action_open", "Open", "Открыть"),
    ("action_back", "← Back", "← Назад"),
    ("action_working", "Working…", "Выполняется…"),

    # --- common labels ------------------------------------------------------------
    ("label_name", "Name", "Название"),
    ("label_status", "Status", "Состояние"),
    ("label_host", "Host", "Хост"),
    ("label_port", "Port", "Порт"),
    ("label_address", "Address", "Адрес"),
    ("label_uptime", "Uptime", "Аптайм"),
    ("label_last_check", "Last check", "Последняя проверка"),
    ("label_cpu", "CPU", "ЦП"),
    ("label_ram", "RAM", "Память"),
    ("label_memory", "Memory", "Память"),
    ("label_disk", "Disk", "Диск"),
    ("label_subject", "Subject", "Объект"),
    ("label_size", "Size", "Размер"),
    ("label_expires", "Expires", "Истекает"),
    ("label_issuer", "Issuer", "Выдан"),
    ("not_measured", "not measured", "не измерено"),
    ("no_measurement", "no measurement", "нет измерений"),
    ("nothing_here_yet", "nothing here yet", "здесь пока пусто"),
    ("could_not_load", "we could not load this", "не удалось загрузить"),

    # --- dashboard ----------------------------------------------------------------
    ("dash_recent_alerts", "Recent alerts", "Недавние оповещения"),
    ("dash_recent_events", "Recent events", "Недавние события"),
    ("dash_problem_servers", "Servers that are not healthy", "Серверы с проблемами"),
    ("dash_needs_attention", "Needs attention", "Требуют внимания"),
    ("dash_preview", "Preview", "Превью"),
    ("dash_all_healthy",
     "Every monitored server and website is within its configured thresholds.",
     "Все серверы и сайты укладываются в заданные пороги."),
    ("dash_nothing_monitored", "Nothing is being monitored yet", "Мониторинг ещё не настроен"),
    ("dash_nothing_monitored_detail",
     "Add a server to start collecting metrics, or add a website to watch its availability and certificate.",
     "Добавьте сервер, чтобы начать собирать метрики, или сайт — чтобы следить за доступностью и сертификатом."),
    ("dash_nothing_happened", "Nothing has happened yet.", "Событий пока не было."),
    ("dash_no_alerts", "No alerts have fired.", "Оповещений не было."),
    ("dash_no_analytics", "No analytics provider is connected", "Провайдер аналитики не подключён"),
    ("dash_no_analytics_detail",
     "Connect Yandex.Metrica and enter a counter ID to see visitors, visits and page views alongside your infrastructure.",
     "Подключите Яндекс.Метрику и укажите номер счётчика, чтобы видеть посетителей, визиты и просмотры рядом с инфраструктурой."),
    ("dash_connect_provider", "Connect a provider", "Подключить провайдер"),

    # --- stat tiles ---------------------------------------------------------------
    ("tile_servers", "Servers", "Серверы"),
    ("tile_online", "Online", "В сети"),
    ("tile_offline", "Offline", "Не в сети"),
    ("tile_websites", "Websites", "Сайты"),
    ("tile_average_cpu", "Average CPU", "Средний ЦП"),
    ("tile_average_ram", "Average RAM", "Средняя память"),
    ("tile_visitors", "Visitors", "Посетители"),
    ("tile_visits", "Visits", "Визиты"),
    ("tile_page_views", "Page views", "Просмотры"),
    ("tile_bounce_rate", "Bounce rate", "Отказы"),

    # --- servers ------------------------------------------------------------------
    ("servers_filter", "Filter by name, host or tag", "Поиск по названию, хосту или метке"),
    ("servers_empty", "No servers yet", "Серверов пока нет"),
    ("servers_empty_detail",
     "Add a Linux server over SSH, or install the agent on it, to start collecting CPU, memory, disk and network metrics.",
     "Добавьте Linux-сервер по SSH или установите на него агент, чтобы собирать метрики ЦП, памяти, диска и сети."),
    ("servers_add", "Add server", "Добавить сервер"),
    ("servers_add_a", "Add a server", "Добавить сервер"),

    # --- server detail ------------------------------------------------------------
    ("tab_overview", "Overview", "Обзор"),
    ("tab_metrics", "Metrics", "Метрики"),
    ("tab_processes", "Processes", "Процессы"),
    ("tab_services", "Services", "Службы"),
    ("tab_docker", "Docker", "Docker"),
    ("tab_websites", "Websites", "Сайты"),
    ("tab_events", "Events", "События"),
    ("tab_settings", "Settings", "Настройки"),
    ("tab_analytics", "Analytics", "Аналитика"),
    ("tab_availability", "Availability", "Доступность"),
    ("tab_ssl", "SSL", "SSL"),
    ("tab_screenshot", "Screenshots", "Скриншот"),
    ("tab_history", "History", "История"),
    ("tab_rules", "Rules", "Правила"),

    ("sd_system", "System", "Система"),
    ("sd_operating_system", "Operating system", "Операционная система"),
    ("sd_kernel", "Kernel", "Ядро"),
    ("sd_architecture", "Architecture", "Архитектура"),
    ("sd_cores", "Cores", "Ядер"),
    ("sd_connection", "Connection", "Подключение"),
    ("sd_top_processes", "Top processes", "Основные процессы"),
    ("sd_sorted_by_cpu", "Sorted by CPU", "По загрузке ЦП"),
    ("sd_no_processes", "No process information was collected.", "Информация о процессах не собрана."),
    ("sd_containers", "Containers", "Контейнеры"),
    ("sd_no_docker", "Docker is not installed on this host", "На этом хосте нет Docker"),
    ("sd_no_docker_detail",
     "Container monitoring appears automatically if Docker is installed later. This does not affect the server's health.",
     "Мониторинг контейнеров появится сам, если Docker установят позже. На состояние сервера это не влияет."),
    ("sd_docker_empty", "Docker is installed, but there are no containers.", "Docker установлен, но контейнеров нет."),
    ("sd_no_systemd", "This host does not use systemd", "На этом хосте нет systemd"),
    ("sd_no_systemd_detail",
     "Service monitoring is only available on systemd-based systems. Everything else about this server is still monitored.",
     "Мониторинг служб доступен только в системах с systemd. Всё остальное по этому серверу собирается как обычно."),
    ("sd_no_websites", "No websites are linked to this server", "К этому серверу не привязан ни один сайт"),
    ("sd_no_websites_detail",
     "Linking a website to the server it runs on lets the app suggest a possible connection between an infrastructure event and a change in traffic.",
     "Привязка сайта к серверу позволяет приложению указывать на возможную связь между событием инфраструктуры и изменением трафика."),
    ("sd_no_events", "Nothing has happened on this server yet.", "Событий по этому серверу пока не было."),

    ("sd_host_key", "Host key", "Ключ хоста"),
    ("sd_host_key_detail",
     "This server's SSH host key was recorded on the first connection and is checked every time since. A key that changes stops the connection.",
     "SSH-ключ этого сервера записан при первом подключении и проверяется при каждом следующем. Если ключ изменится, подключение прервётся."),
    ("sd_host_key_warning",
     "Forget it only after a host was rebuilt or its keys were regenerated — and check the new fingerprint against the server before reconnecting.",
     "Забывайте ключ только после переустановки хоста или смены его ключей — и сверьте новый отпечаток с сервером перед подключением."),
    ("sd_forget_host_key", "Forget host key", "Забыть ключ хоста"),
    ("sd_forget_confirm", "Forget the recorded key?", "Забыть записанный ключ?"),
    ("sd_forget_it", "Forget it", "Забыть"),
    ("sd_remove_server", "Remove server", "Удалить сервер"),
    ("sd_remove_detail",
     "Removes this server, its stored credential and its metric history. The server itself is not touched — nothing is installed on it to remove.",
     "Удаляет сервер, его сохранённые учётные данные и историю метрик. Сам сервер не затрагивается — на нём ничего не установлено."),
    ("sd_remove_confirm_prefix", "Remove ", "Удалить "),
    ("sd_remove_confirm_suffix", " and its history?", " вместе с историей?"),

    # --- websites -----------------------------------------------------------------
    ("websites_empty", "No websites yet", "Сайтов пока нет"),
    ("websites_empty_detail",
     "Add a URL to monitor its availability, response time and TLS certificate — and, if you connect an analytics provider, its traffic.",
     "Добавьте адрес, чтобы следить за доступностью, временем ответа и TLS-сертификатом — а если подключить аналитику, то и за трафиком."),
    ("websites_add", "Add website", "Добавить сайт"),
    ("websites_add_a", "Add a website", "Добавить сайт"),
    ("websites_grid", "Grid", "Плитка"),
    ("websites_list", "List", "Список"),
    ("wd_http_status", "HTTP status", "Код HTTP"),
    ("wd_response_time", "Response time", "Время ответа"),
    ("wd_uptime_24h", "Uptime (24h)", "Доступность (24 ч)"),
    ("wd_tls_certificate", "TLS certificate", "TLS-сертификат"),
    ("wd_top_pages", "Top pages", "Популярные страницы"),
    ("wd_no_events", "Nothing has happened for this website yet.", "Событий по этому сайту пока не было."),
    ("wd_no_analytics",
     "Connect Yandex.Metrica in Settings to see visitors, visits and page views for this website.",
     "Подключите Яндекс.Метрику в настройках, чтобы видеть посетителей, визиты и просмотры этого сайта."),
    ("wd_refresh_screenshot", "Refresh screenshot", "Обновить скриншот"),
    ("wd_offline_no_shot", "Website is currently offline", "Сайт сейчас недоступен"),

    # --- analytics ----------------------------------------------------------------
    ("analytics_by_website", "By website", "По сайтам"),
    ("analytics_vs_previous", "vs previous period", "к прошлому периоду"),

    # --- alerts -------------------------------------------------------------------
    ("alerts_nothing_firing", "Nothing is firing", "Ничего не сработало"),
    ("alerts_acknowledge", "Acknowledge", "Принять"),
    ("alerts_acknowledged", "Acknowledged", "Принято"),
    ("alerts_add_rule", "Add rule", "Добавить правило"),
    ("alerts_open", "open", "открыт"),
    ("alerts_resolved", "resolved", "закрыт"),

    # --- settings -----------------------------------------------------------------
    ("set_appearance", "Appearance", "Внешний вид"),
    ("set_theme", "Theme", "Тема"),
    ("set_language", "Language", "Язык"),
    ("set_notifications", "Notifications", "Уведомления"),
    ("set_desktop_notifications", "Desktop notifications", "Уведомления на рабочем столе"),
    ("set_play_sound", "Play a sound", "Звуковой сигнал"),
    ("set_webhook_url", "Webhook URL", "Адрес вебхука"),
    ("set_screenshots", "Screenshots", "Скриншоты"),
    ("set_no_browser",
     "No Chromium-family browser was found, so website previews are unavailable. Install Chrome, Chromium, Edge or Brave, or set a browser path in the configuration file.",
     "Браузер на основе Chromium не найден, поэтому превью сайтов недоступны. Установите Chrome, Chromium, Edge или Brave — либо укажите путь к браузеру в файле конфигурации."),
    ("set_counter_id", "Counter ID", "Номер счётчика"),
    ("set_oauth_token", "OAuth token", "OAuth-токен"),
    ("set_token_hint", "Stored in the system keychain, never in the database",
     "Хранится в системном хранилище учётных данных, а не в базе"),
    ("set_credential_storage", "Credential storage", "Хранилище учётных данных"),
    ("set_backend", "Backend", "Хранилище"),
    ("set_encrypted_file_warning",
     "Credentials are stored in an encrypted file because no system keystore was available on this machine. They are encrypted, but a system keystore is stronger.",
     "Учётные данные хранятся в зашифрованном файле, потому что системное хранилище на этой машине недоступно. Шифрование надёжное, но системное хранилище безопаснее."),
    ("set_storage_diagnostics", "Storage and diagnostics", "Хранение и диагностика"),
    ("set_database", "Database", "База данных"),
    ("set_logs", "Logs", "Журналы"),
    ("set_debug_mode", "Debug mode (verbose logging and the scheduler panel)",
     "Режим отладки (подробные журналы и панель планировщика)"),

    # --- dialogs ------------------------------------------------------------------
    ("dlg_add_server", "Add server", "Добавление сервера"),
    ("dlg_add_website", "Add website", "Добавление сайта"),
    ("dlg_ssh_subtitle", "Credentials are stored in the system keychain, never in the database.",
     "Учётные данные сохраняются в системном хранилище, а не в базе."),
    ("dlg_agent_subtitle",
     "The agent must already be installed on this host. Its installer printed the token.",
     "Агент уже должен быть установлен на этом хосте. Его установщик напечатал токен."),
    ("dlg_website_subtitle",
     "Checked for DNS, connection, HTTP status, response time and certificate expiry.",
     "Проверяются DNS, подключение, код HTTP, время ответа и срок действия сертификата."),
    ("dlg_connect_via", "Connect via", "Подключение"),
    ("dlg_mode_ssh", "SSH (no agent required)", "SSH (агент не нужен)"),
    ("dlg_mode_agent", "Agent (HTTPS)", "Агент (HTTPS)"),
    ("dlg_username", "Username", "Пользователь"),
    ("dlg_authentication", "Authentication", "Аутентификация"),
    ("dlg_auth_password", "Password", "Пароль"),
    ("dlg_auth_key", "Private key", "Приватный ключ"),
    ("dlg_auth_encrypted_key", "Encrypted private key", "Зашифрованный приватный ключ"),
    ("dlg_passphrase", "Passphrase", "Парольная фраза"),
    ("dlg_token", "Token", "Токен"),
    ("dlg_poll_every", "Poll every", "Опрашивать раз в"),
    ("dlg_check_every", "Check every", "Проверять раз в"),
    ("dlg_expected_status", "Expected status", "Ожидаемый код"),
    ("dlg_expected_text", "Expected text", "Ожидаемый текст"),
    ("dlg_url", "URL", "Адрес"),
    ("dlg_ph_server_name", "prod-web-01", "prod-web-01"),
    ("dlg_ph_host", "10.0.0.5 or web01.example.com", "10.0.0.5 или web01.example.com"),
    ("dlg_ph_username", "vds-monitor", "vds-monitor"),
    ("dlg_ph_password", "The account's password", "Пароль этой учётной записи"),
    ("dlg_ph_key", "Paste the key, including the BEGIN and END lines",
     "Вставьте ключ целиком, вместе со строками BEGIN и END"),
    ("dlg_ph_passphrase", "The passphrase protecting that key", "Парольная фраза от этого ключа"),
    ("dlg_ph_token", "Printed by the agent's installer", "Напечатан установщиком агента"),
    ("dlg_ph_seconds", "seconds", "секунд"),
    ("dlg_ph_website_name", "Company website", "Сайт компании"),
    ("dlg_ph_url", "example.com", "example.com"),
    ("dlg_ph_expected_text", "Optional — a substring the page must contain",
     "Необязательно — подстрока, которая должна быть на странице"),
    ("dlg_ph_counter", "e.g. 12345678", "например, 12345678"),
    ("dlg_ph_webhook", "https://hooks.example.com/…", "https://hooks.example.com/…"),
    ("dlg_scheme_hint", "https:// is assumed when no scheme is given.",
     "Если схема не указана, подставляется https://."),
    ("dlg_expected_text_hint",
     "Leave the text blank to check only the status code. A blank expectation matches every response, which is a check that looks like it passes and does nothing.",
     "Оставьте текст пустым, чтобы проверять только код ответа. Пустое ожидание совпадает с любым ответом — такая проверка выглядит успешной, но ничего не проверяет."),
    ("dlg_fingerprint_hint",
     "The agent's certificate fingerprint is shown for confirmation on the first connection. Check it against what the installer printed.",
     "При первом подключении приложение покажет отпечаток сертификата агента. Сверьте его с тем, что напечатал установщик."),
    # --- statuses -----------------------------------------------------------------
    ("status_online", "Online", "В сети"),
    ("status_warning", "Warning", "Предупреждение"),
    ("status_critical", "Critical", "Критично"),
    ("status_offline", "Offline", "Не в сети"),
    ("status_unknown", "Unknown", "Неизвестно"),

    # --- themes -------------------------------------------------------------------
    ("theme_light", "Light", "Светлая"),
    ("theme_dark", "Dark", "Тёмная"),
    ("theme_system", "System", "Как в системе"),

    # --- time ranges --------------------------------------------------------------
    ("range_1h", "1 hour", "1 час"),
    ("range_6h", "6 hours", "6 часов"),
    ("range_24h", "24 hours", "24 часа"),
    ("range_7d", "7 days", "7 дней"),
    ("range_30d", "30 days", "30 дней"),
    ("range_90d", "90 days", "90 дней"),
    ("range_1y", "1 year", "1 год"),

    # --- analytics periods --------------------------------------------------------
    ("period_today", "Today", "Сегодня"),
    ("period_yesterday", "Yesterday", "Вчера"),
    ("period_7d", "7 days", "7 дней"),
    ("period_30d", "30 days", "30 дней"),
    ("period_90d", "90 days", "90 дней"),

    # --- analytics metrics --------------------------------------------------------
    ("am_visitors", "Visitors", "Посетители"),
    ("am_visits", "Visits", "Визиты"),
    ("am_page_views", "Page views", "Просмотры"),
    ("am_sessions", "Sessions", "Сессии"),
    ("am_unique_visitors", "Unique visitors", "Уникальные посетители"),
    ("am_new_visitors", "New visitors", "Новые посетители"),
    ("am_returning_visitors", "Returning visitors", "Вернувшиеся посетители"),
    ("am_bounce_rate", "Bounce rate", "Отказы"),
    ("am_session_duration", "Avg. session duration", "Средняя длительность сессии"),
    ("am_pages_per_session", "Pages per session", "Страниц за сессию"),

    # --- screenshot refresh policies ----------------------------------------------
    ("policy_hourly", "Every hour", "Каждый час"),
    ("policy_six_hours", "Every 6 hours", "Каждые 6 часов"),
    ("policy_daily", "Every 24 hours", "Раз в сутки"),
    ("policy_manual", "Manual", "Вручную"),

    # --- metric kinds -------------------------------------------------------------
    ("mk_cpu", "CPU", "ЦП"),
    ("mk_ram", "RAM", "Память"),
    ("mk_ram_used", "RAM used", "Занято памяти"),
    ("mk_swap", "Swap", "Подкачка"),
    ("mk_disk", "Disk", "Диск"),
    ("mk_disk_used", "Disk used", "Занято на диске"),
    ("mk_network_in", "Network in", "Сеть, приём"),
    ("mk_network_out", "Network out", "Сеть, передача"),
    ("mk_load_1m", "Load 1m", "Нагрузка 1 мин"),
    ("mk_load_5m", "Load 5m", "Нагрузка 5 мин"),
    ("mk_load_15m", "Load 15m", "Нагрузка 15 мин"),
    ("mk_uptime", "Uptime", "Аптайм"),
    ("mk_processes", "Processes", "Процессы"),
    ("mk_temperature", "Temperature", "Температура"),
    ("mk_response_time", "Response time", "Время ответа"),
    ("mk_ssl_expiry", "SSL expiry", "Срок сертификата"),

    # --- relative time ------------------------------------------------------------
    # `{}` is the number. Russian keeps the unit separate from the digit, which English
    # does not, so these cannot be assembled from parts.
    ("time_never", "never", "никогда"),
    ("time_just_now", "just now", "только что"),
    ("time_secs_ago", "{}s ago", "{} с назад"),
    ("time_mins_ago", "{}m ago", "{} мин назад"),
    ("time_hours_ago", "{}h ago", "{} ч назад"),
    ("time_days_ago", "{}d ago", "{} дн назад"),

    # --- durations ----------------------------------------------------------------
    ("dur_days_hours", "{}d {}h", "{} д {} ч"),
    ("dur_hours_mins", "{}h {}m", "{} ч {} мин"),
    ("dur_mins", "{}m", "{} мин"),
    ("dur_secs", "{}s", "{} с"),

    # --- certificate expiry -------------------------------------------------------
    ("ssl_expired_days_ago", "expired {} days ago", "истёк {} дн назад"),
    ("ssl_expires_today", "expires today", "истекает сегодня"),
    ("ssl_one_day", "1 day", "1 день"),
    ("ssl_days", "{} days", "{} дн"),

    # --- website card lines -------------------------------------------------------
    ("card_response", "Response: {}", "Ответ: {}"),
    ("card_ssl", "SSL: {}", "SSL: {}"),
    ("card_uptime_24h", "Uptime 24h: {}", "Доступность за 24 ч: {}"),
    ("card_visitors_today", "Visitors today: {}", "Посетителей сегодня: {}"),
    ("card_analytics_updated", "Analytics updated {}", "Аналитика обновлена {}"),

    # --- screenshot presentation --------------------------------------------------
    ("shot_captured", "Captured {}", "Снято {}"),
    ("shot_capturing", "Capturing…", "Съёмка…"),
    ("shot_none_yet", "No screenshot yet", "Скриншота ещё нет"),
    ("shot_offline", "Screenshot unavailable — the website is currently offline",
     "Скриншот недоступен — сайт сейчас не отвечает"),
    ("shot_failed", "Screenshot generation failed: {}", "Не удалось сделать скриншот: {}"),
    ("shot_unsupported", "Screenshots are not available on this machine",
     "Скриншоты на этой машине недоступны"),

    # --- event feed ---------------------------------------------------------------
    ("ev_server_status", "Server went from {} to {}", "Сервер: {} → {}"),
    ("ev_collection_failed", "Collection failed ({} in a row): {}",
     "Сбор не удался ({} раз подряд): {}"),
    ("ev_website_status", "Website went from {} to {}", "Сайт: {} → {}"),
    ("ev_threshold", "{} reached {}, above {}", "{} достигло {}, порог {}"),
    ("ev_certificate", "Certificate {}", "Сертификат {}"),
    ("ev_traffic_anomaly", "Traffic anomaly: {} changed by {}",
     "Аномалия трафика: {} изменилось на {}"),
    ("ev_analytics_refreshed", "Analytics refreshed", "Аналитика обновлена"),
    ("ev_analytics_failed", "Analytics refresh failed: {}", "Не удалось обновить аналитику: {}"),
    ("ev_screenshot_updated", "Screenshot updated", "Скриншот обновлён"),
    ("ev_screenshot_failed", "Screenshot failed: {}", "Не удалось снять скриншот: {}"),
    ("ev_incident_resolved", "Incident resolved", "Инцидент закрыт"),
    ("ev_container_state", "Container {} is {}", "Контейнер {}: {}"),
    ("ev_service_state", "Service {} is {}", "Служба {}: {}"),
    ("ev_website_checked", "Website checked", "Сайт проверен"),
    ("ev_metrics_collected", "Collected {} metrics", "Собрано метрик: {}"),
    # The audit trail for the one part of the product that writes to a server.
    ("ev_file_written", "File saved: {}", "Файл сохранён: {}"),
    ("ev_file_deleted", "File deleted: {}", "Файл удалён: {}"),
    ("ev_file_dir_created", "Folder created: {}", "Папка создана: {}"),

    # --- incident rows ------------------------------------------------------------
    ("incident_open_for", "Open for {}", "Открыт {}"),
    # --- validation messages ------------------------------------------------------
    # Shown in a dialog when a form is rejected. The domain's own `Display` is English
    # and belongs to the domain; turning it into something the user reads is the
    # presentation layer's job, which is why these live here.
    ("err_server_name_empty", "Enter a name for the server", "Укажите название сервера"),
    ("err_server_host_empty", "Enter the server's address", "Укажите адрес сервера"),
    ("err_port_invalid", "The port must be between 1 and 65535", "Порт должен быть от 1 до 65535"),
    ("err_interval_invalid", "The interval must be at least 1 second",
     "Интервал должен быть не меньше 1 секунды"),
    ("err_failures_invalid", "The failure threshold must be at least 1 check",
     "Порог должен быть не меньше одной неудачной проверки"),
    ("err_timeout_invalid", "The timeout must be at least 1 second",
     "Таймаут должен быть не меньше 1 секунды"),
    ("err_timeout_too_long",
     "The timeout must not exceed four polling intervals, or checks will pile up",
     "Таймаут не должен превышать четыре интервала опроса, иначе проверки начнут накапливаться"),
    ("err_thresholds_inverted", "The warning and critical thresholds are the wrong way round",
     "Пороги предупреждения и критического уровня перепутаны местами"),

    ("err_website_name_empty", "Enter a name for the website", "Укажите название сайта"),
    ("err_url_malformed", "That address is not a valid URL", "Это не похоже на корректный адрес"),
    ("err_url_scheme", "Only http and https addresses are monitored",
     "Отслеживаются только адреса http и https"),
    ("err_url_no_host", "The address has no host name", "В адресе нет имени хоста"),
    ("err_status_invalid", "The expected status must be between 100 and 599",
     "Ожидаемый код должен быть от 100 до 599"),

    ("err_credential_missing", "Enter the password, key or token for this connection",
     "Введите пароль, ключ или токен для этого подключения"),
    ("err_credential_store", "The credential could not be saved: {}",
     "Не удалось сохранить учётные данные: {}"),
    ("err_save_failed", "Could not save: {}", "Не удалось сохранить: {}"),

    ("set_analytics_hint",
     "The token is entered once and covers every counter the account can see. Each website’s counter number is set on its own Analytics tab.",
     "Токен вводится один раз и покрывает все счётчики аккаунта. Номер счётчика каждого сайта указывается на его вкладке «Аналитика»."),
    ("set_token_stored", "Token saved", "Токен сохранён"),

    ("wd_connect_analytics", "Connect Yandex.Metrica", "Подключить Яндекс.Метрику"),
    ("wd_counter_hint", "The counter number for this website — digits only",
     "Номер счётчика этого сайта — только цифры"),
    ("wd_counter", "Counter", "Счётчик"),
    ("action_connect", "Connect", "Подключить"),
    ("action_disconnect", "Disconnect", "Отключить"),

    # --- why a collection failed --------------------------------------------------
    # Shown on a server that is not answering. The detail from the transport is kept
    # beneath these, because "which key, exactly" is what makes a failure diagnosable.
    ("servers_edit", "Edit server", "Изменить сервер"),
    ("websites_edit", "Edit website", "Изменить сайт"),
    ("action_edit", "Edit", "Изменить"),
    ("action_save_changes", "Save", "Сохранить"),
    ("dlg_ph_secret_kept", "Leave empty to keep the stored one",
     "Оставьте пустым, чтобы сохранить прежний"),

    ("conn_auth", "Authentication failed. Check the user name and the key or password.",
     "Аутентификация не удалась. Проверьте имя пользователя и ключ или пароль."),
    ("conn_host_key", "The server’s host key has changed. Verify the new fingerprint before reconnecting.",
     "Ключ хоста сервера изменился. Сверьте новый отпечаток, прежде чем подключаться."),
    ("conn_refused", "Could not connect. The server may be down, or a firewall is in the way.",
     "Не удалось подключиться. Сервер выключен либо мешает межсетевой экран."),
    ("conn_timeout", "The server did not answer in time.",
     "Сервер не ответил вовремя."),
    ("conn_command", "A command did not run on the server.",
     "Команда на сервере не выполнилась."),
    ("conn_disconnected", "The connection was lost.", "Соединение потеряно."),
    ("conn_no_credential", "The stored credential could not be read.",
     "Не удалось прочитать сохранённые учётные данные."),
    ("conn_protocol", "The server answered in a way this version does not understand.",
     "Сервер ответил так, как эта версия не понимает."),

    ("err_counter_empty", "Enter the counter number", "Укажите номер счётчика"),
    ("err_counter_malformed", "A counter number is digits only — not a link",
     "Номер счётчика — только цифры, не ссылка"),
    ("err_no_analytics_token", "Save the OAuth token in Settings first",
     "Сначала сохраните OAuth-токен в настройках"),
    # The mistake the application page invites: its 32-character identifier is the most
    # prominent string on the screen, and the token is two steps further on.
    ("err_token_is_app_id",
     "That is the application ID, not the token. The token is longer, begins with y0_, "
     "and is issued by the authorisation link — not shown on the application's page.",
     "Это ID приложения, а не токен. Токен длиннее, начинается с y0_ и выдаётся по ссылке "
     "авторизации — на странице приложения его нет."),

    # Stable codes from `ProviderError::kind`. A user whose token expired needs to be told
    # that, in their language — not shown the provider's own `Invalid oauth_token`.
    ("prov_authentication",
     "Yandex will not accept this token. Check that what you saved is the OAuth token "
     "itself and not the application ID, then get a fresh one if it is.",
     "Яндекс не принимает этот токен. Проверьте, что сохранён именно OAuth-токен, "
     "а не ID приложения, и при необходимости получите новый."),
    ("prov_forbidden",
     "The token is valid but has no access to this counter. Check that it belongs to the "
     "account the counter is on.",
     "Токен рабочий, но доступа к этому счётчику нет. Проверьте, что он от того же аккаунта, "
     "где заведён счётчик."),
    ("prov_not_found", "The provider does not know this counter number",
     "Провайдер не знает такого номера счётчика"),
    ("prov_rate_limited", "The provider is asking us to slow down; it will retry shortly",
     "Провайдер просит сбавить темп — повтор будет чуть позже"),
    ("prov_rejected", "The provider rejected the request", "Провайдер отклонил запрос"),
    ("prov_upstream", "The provider returned an error", "Провайдер вернул ошибку"),
    ("prov_network", "Could not reach the provider", "Не удалось связаться с провайдером"),
    ("prov_timeout", "The provider did not answer in time", "Провайдер не ответил вовремя"),
    ("prov_malformed", "The provider's answer could not be read",
     "Не удалось разобрать ответ провайдера"),
    ("prov_unsupported", "This provider does not offer that", "Провайдер такого не умеет"),
    ("prov_missing_credential", "No token is saved for this provider",
     "Для этого провайдера не сохранён токен"),

    # --- file manager -------------------------------------------------------------
    # The only screen that changes anything on a server, so its wording is deliberately
    # plain about what is about to happen.
    ("nav_files", "Files", "Файлы"),
    ("files_title", "Files", "Файлы"),
    ("files_subtitle", "Browse and edit files on the server",
     "Просмотр и редактирование файлов на сервере"),
    ("files_site_folders", "Site folders", "Папки сайтов"),
    ("files_no_site_folders", "No site folders found in the web server configuration",
     "В конфигурации веб-сервера папки сайтов не найдены"),
    ("files_up", "Up a level", "На уровень вверх"),
    ("files_empty", "This folder is empty", "Папка пуста"),
    ("files_loading", "Loading…", "Загрузка…"),
    ("files_new_folder", "New folder", "Новая папка"),
    ("files_new_file", "New file", "Новый файл"),
    ("files_folder_name", "Folder name", "Название папки"),
    ("files_file_name", "File name", "Имя файла"),
    ("files_edit", "Edit", "Изменить"),
    ("files_delete_title", "Delete", "Удалить"),
    ("files_delete_confirm", "Delete {}? This cannot be undone.",
     "Удалить {}? Отменить это будет нельзя."),
    ("files_delete_folder_note", "Only an empty folder can be deleted.",
     "Удалить можно только пустую папку."),
    ("files_saved", "Saved", "Сохранено"),
    ("files_unsaved", "Unsaved changes", "Есть несохранённые изменения"),
    ("files_truncated",
     "Only the beginning of this file is shown — it is too large to edit here safely.",
     "Показано только начало файла — он слишком велик, чтобы редактировать его здесь."),
    ("files_read_only", "Read-only", "Только чтение"),
    ("files_modified", "Modified", "Изменён"),
    ("files_owner", "Owner", "Владелец"),
    ("files_permissions", "Permissions", "Права"),
    ("files_path", "Path", "Путь"),
    ("files_link_to", "link to {}", "ссылка на {}"),
    # A preview says what a file is when it cannot show it, because "4 MB, not text" is a
    # useful answer and a window full of mojibake is not.
    ("files_binary", "Not a text file — nothing to show here",
     "Не текстовый файл — показывать нечего"),
    ("files_image_too_large", "The image is too large to preview",
     "Изображение слишком велико для просмотра"),
    ("files_image_broken", "The image could not be decoded",
     "Не удалось прочитать изображение"),
    ("files_image_size", "{} × {}", "{} × {}"),
    ("files_items", "{} items", "объектов: {}"),

    # Stable codes from `FileError::kind`, translated here because a formatted English
    # sentence cannot be translated after it exists.
    ("err_file_not_found", "No such file or folder", "Файл или папка не найдены"),
    ("err_file_denied", "You do not have permission to do that on this server",
     "На этом сервере нет прав на это действие"),
    ("err_file_not_a_directory", "That is not a folder", "Это не папка"),
    ("err_file_not_a_file", "That is not a file", "Это не файл"),
    ("err_file_not_text", "This is not a text file, so it cannot be shown or edited",
     "Это не текстовый файл — показать или изменить его нельзя"),
    ("err_file_too_large", "The file is too large to open here",
     "Файл слишком велик, чтобы открыть его здесь"),
    ("err_file_malformed", "The server's answer could not be read: {}",
     "Не удалось разобрать ответ сервера: {}"),]


def slint_global():
    lines = [
        "// Every user-facing string in the interface.",
        "//",
        "// Generated alongside `apps/ui/src/i18n.rs` from one table, so a key cannot exist on",
        "// one side and not the other. Do not edit by hand: change the table and regenerate.",
        "//",
        "// Screens reference `L.key` and never a literal, which is what makes the language",
        "// switch a single assignment rather than an audit of two thousand lines of markup.",
        "",
        "export global L {",
    ]
    for key, en, _ru in ENTRIES:
        escaped = en.replace('\\', '\\\\').replace('"', '\\"')
        lines.append(f'    in property <string> {key}: "{escaped}";')
    lines.append("}")
    lines.append("")
    lines.append("")
    return "\n".join(lines)


def rust_catalogue():
    def lit(text):
        return '"' + text.replace('\\', '\\\\').replace('"', '\\"') + '"'

    lines = [
        "//! The interface's strings, in every language it speaks.",
        "//!",
        "//! Generated from one table together with `ui/strings.slint`, so the two cannot",
        "//! disagree: a key added on one side without the other stops compiling.",
        "//!",
        "//! ## Why a catalogue rather than `@tr()`",
        "//!",
        "//! Slint can use gettext, which would mean `.po` files and a system gettext library.",
        "//! That is a build dependency this project has gone out of its way not to need —",
        "//! see `docs/adr/001-technology-stack.md`. A plain global costs a generated file and",
        "//! gives the compiler the chance to catch a missing string, which `@tr()` does not.",
        "",
        "use crate::{AppWindow, L};",
        "use slint::ComponentHandle;",
        "",
        "/// The languages the interface speaks.",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub enum Language {",
        "    English,",
        "    Russian,",
        "}",
        "",
        "impl Language {",
        "    /// The code stored in the configuration file.",
        "    pub fn as_str(self) -> &'static str {",
        "        match self {",
        "            Language::English => \"en\",",
        "            Language::Russian => \"ru\",",
        "        }",
        "    }",
        "",
        "    /// How the language is named *in that language*, for the picker.",
        "    ///",
        "    /// A Russian speaker looking for their language should not have to recognise the",
        "    /// word \"Russian\" first.",
        "    pub fn endonym(self) -> &'static str {",
        "        match self {",
        "            Language::English => \"English\",",
        "            Language::Russian => \"Русский\",",
        "        }",
        "    }",
        "",
        "    /// Every language, in the order the picker shows them.",
        "    pub const ALL: &'static [Language] = &[Language::English, Language::Russian];",
        "",
        "    /// Resolves the configured value, which may be a code or `\"system\"`.",
        "    ///",
        "    /// Anything unrecognised falls back to the system's choice rather than failing:",
        "    /// a typo in a configuration file must not leave the application without words.",
        "    pub fn resolve(configured: &str) -> Language {",
        "        match configured.trim().to_ascii_lowercase().as_str() {",
        "            \"en\" | \"english\" => Language::English,",
        "            \"ru\" | \"russian\" => Language::Russian,",
        "            _ => Language::from_system(),",
        "        }",
        "    }",
        "",
        "    /// What the operating system says the user prefers.",
        "    ///",
        "    /// Read from the usual environment variables, which is enough on Linux and macOS.",
        "    /// Windows does not set them, so a Windows user gets English until they choose;",
        "    /// the picker is one click away and the choice is remembered.",
        "    pub fn from_system() -> Language {",
        "        for key in [\"LC_ALL\", \"LC_MESSAGES\", \"LANG\"] {",
        "            if let Ok(value) = std::env::var(key) {",
        "                let value = value.to_ascii_lowercase();",
        "                if value.starts_with(\"ru\") {",
        "                    return Language::Russian;",
        "                }",
        "                if !value.is_empty() && value != \"c\" && value != \"posix\" {",
        "                    return Language::English;",
        "                }",
        "            }",
        "        }",
        "        Language::English",
        "    }",
        "",
        "    /// The index of this language in [`Language::ALL`], for the picker.",
        "    pub fn index(self) -> i32 {",
        "        Language::ALL",
        "            .iter()",
        "            .position(|candidate| *candidate == self)",
        "            .and_then(|index| i32::try_from(index).ok())",
        "            .unwrap_or(0)",
        "    }",
        "",
        "    /// Resolves a picker index back to a language.",
        "    pub fn at(index: i32) -> Language {",
        "        usize::try_from(index)",
        "            .ok()",
        "            .and_then(|index| Language::ALL.get(index))",
        "            .copied()",
        "            .unwrap_or(Language::English)",
        "    }",
        "",
        "    pub fn strings(self) -> Strings {",
        "        match self {",
        "            Language::English => Strings::english(),",
        "            Language::Russian => Strings::russian(),",
        "        }",
        "    }",
        "}",
        "",
        "/// Every string the interface shows.",
        "#[derive(Debug, Clone, Copy, PartialEq, Eq)]",
        "pub struct Strings {",
    ]
    for key, _en, _ru in ENTRIES:
        lines.append(f"    pub {key}: &'static str,")
    lines.append("}")
    lines.append("")
    lines.append("impl Strings {")
    lines.append("    pub fn english() -> Self {")
    lines.append("        Self {")
    for key, en, _ru in ENTRIES:
        lines.append(f"            {key}: {lit(en)},")
    lines.append("        }")
    lines.append("    }")
    lines.append("")
    lines.append("    pub fn russian() -> Self {")
    lines.append("        Self {")
    for key, _en, ru in ENTRIES:
        lines.append(f"            {key}: {lit(ru)},")
    lines.append("        }")
    lines.append("    }")
    lines.append("}")
    lines.append("")
    lines.append("/// The language the process is currently speaking.")
    lines.append("///")
    lines.append("/// Process-global because it genuinely is: every formatted string in the")
    lines.append("/// interface is in one language at a time, and threading a `&Strings` through")
    lines.append("/// `format::relative_time` and each of its callers would put a parameter on forty")
    lines.append("/// functions to express a fact that never varies between them.")
    lines.append("///")
    lines.append("/// Stored as an index into [`Language::ALL`] so both the UI thread and the worker")
    lines.append("/// can read it without a lock.")
    lines.append("static CURRENT: std::sync::atomic::AtomicUsize =")
    lines.append("    std::sync::atomic::AtomicUsize::new(0);")
    lines.append("")
    lines.append("/// Sets the language every later formatting call will use.")
    lines.append("pub fn set_current(language: Language) {")
    lines.append("    let index = usize::try_from(language.index()).unwrap_or(0);")
    lines.append("    CURRENT.store(index, std::sync::atomic::Ordering::Relaxed);")
    lines.append("}")
    lines.append("")
    lines.append("/// The language in force.")
    lines.append("pub fn current() -> Language {")
    lines.append("    let index = CURRENT.load(std::sync::atomic::Ordering::Relaxed);")
    lines.append("    Language::ALL.get(index).copied().unwrap_or(Language::English)")
    lines.append("}")
    lines.append("")
    lines.append("/// The catalogue in force.")
    lines.append("pub fn strings() -> Strings {")
    lines.append("    current().strings()")
    lines.append("}")
    lines.append("")
    lines.append("/// Pushes a catalogue into the window.")
    lines.append("///")
    lines.append("/// Must run on the UI thread. Called once at startup and again whenever the")
    lines.append("/// language changes — Slint re-renders every binding that reads a changed")
    lines.append("/// property, so switching takes effect without a restart.")
    lines.append("pub fn apply(window: &AppWindow, strings: &Strings) {")
    lines.append("    let global = window.global::<L>();")
    for key, _en, _ru in ENTRIES:
        lines.append(f"    global.set_{key}(strings.{key}.into());")
    lines.append("}")
    lines.append("")
    lines.append("#[cfg(test)]")
    lines.append("mod tests {")
    lines.append("    use super::*;")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn every_string_is_translated_in_every_language() {")
    lines.append("        // An empty string would render as a blank label rather than as an obvious")
    lines.append("        // mistake, so it is caught here instead of on screen.")
    lines.append("        for language in Language::ALL {")
    lines.append("            let strings = language.strings();")
    lines.append("            let rendered = format!(\"{strings:?}\");")
    lines.append("            assert!(")
    lines.append("                !rendered.contains(\": \\\"\\\"\"),")
    lines.append("                \"{} has an empty string\",")
    lines.append("                language.as_str()")
    lines.append("            );")
    lines.append("        }")
    lines.append("    }")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn placeholders_survive_translation() {")
    lines.append("        // A translation that drops its `{}` silently loses the number it was")
    lines.append("        // meant to carry, which is the one localisation bug nobody spots.")
    lines.append("        let english = Strings::english();")
    lines.append("        let russian = Strings::russian();")
    lines.append("")
    lines.append("        let pairs: [(&str, &str, &str); 14] = [")
    lines.append("            (\"time_secs_ago\", english.time_secs_ago, russian.time_secs_ago),")
    lines.append("            (\"time_mins_ago\", english.time_mins_ago, russian.time_mins_ago),")
    lines.append("            (\"time_hours_ago\", english.time_hours_ago, russian.time_hours_ago),")
    lines.append("            (\"time_days_ago\", english.time_days_ago, russian.time_days_ago),")
    lines.append("            (\"dur_mins\", english.dur_mins, russian.dur_mins),")
    lines.append("            (\"dur_secs\", english.dur_secs, russian.dur_secs),")
    lines.append("            (\"ssl_days\", english.ssl_days, russian.ssl_days),")
    lines.append("            (\"ssl_expired_days_ago\", english.ssl_expired_days_ago, russian.ssl_expired_days_ago),")
    lines.append("            (\"card_response\", english.card_response, russian.card_response),")
    lines.append("            (\"card_ssl\", english.card_ssl, russian.card_ssl),")
    lines.append("            (\"card_uptime_24h\", english.card_uptime_24h, russian.card_uptime_24h),")
    lines.append("            (\"card_visitors_today\", english.card_visitors_today, russian.card_visitors_today),")
    lines.append("            (\"shot_captured\", english.shot_captured, russian.shot_captured),")
    lines.append("            (\"incident_open_for\", english.incident_open_for, russian.incident_open_for),")
    lines.append("        ];")
    lines.append("")
    lines.append("        for (key, en, ru) in pairs {")
    lines.append("            assert!(en.contains(\"{}\"), \"{key} lost its placeholder in English\");")
    lines.append("            assert!(ru.contains(\"{}\"), \"{key} lost its placeholder in Russian\");")
    lines.append("        }")
    lines.append("    }")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn two_placeholder_strings_keep_both() {")
    lines.append("        for strings in [Strings::english(), Strings::russian()] {")
    lines.append("            assert_eq!(strings.dur_days_hours.matches(\"{}\").count(), 2);")
    lines.append("            assert_eq!(strings.dur_hours_mins.matches(\"{}\").count(), 2);")
    lines.append("            assert_eq!(strings.ev_server_status.matches(\"{}\").count(), 2);")
    lines.append("            assert_eq!(strings.ev_container_state.matches(\"{}\").count(), 2);")
    lines.append("        }")
    lines.append("    }")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn a_configured_code_wins_over_the_system() {")
    lines.append("        assert_eq!(Language::resolve(\"ru\"), Language::Russian);")
    lines.append("        assert_eq!(Language::resolve(\"en\"), Language::English);")
    lines.append("        assert_eq!(Language::resolve(\"RU\"), Language::Russian);")
    lines.append("        assert_eq!(Language::resolve(\" ru \"), Language::Russian);")
    lines.append("    }")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn an_unknown_code_falls_back_rather_than_leaving_the_app_wordless() {")
    lines.append("        // A typo in a configuration file must not produce a blank interface.")
    lines.append("        let fallback = Language::from_system();")
    lines.append("        assert_eq!(Language::resolve(\"klingon\"), fallback);")
    lines.append("        assert_eq!(Language::resolve(\"\"), fallback);")
    lines.append("        assert_eq!(Language::resolve(\"system\"), fallback);")
    lines.append("    }")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn the_picker_round_trips() {")
    lines.append("        for language in Language::ALL {")
    lines.append("            assert_eq!(Language::at(language.index()), *language);")
    lines.append("        }")
    lines.append("        // Out of range must not panic; the index comes from the view.")
    lines.append("        assert_eq!(Language::at(-1), Language::English);")
    lines.append("        assert_eq!(Language::at(99), Language::English);")
    lines.append("    }")
    lines.append("")
    lines.append("    #[test]")
    lines.append("    fn every_language_names_itself_in_itself() {")
    lines.append("        // So a speaker can find their language without first reading another.")
    lines.append("        assert_eq!(Language::Russian.endonym(), \"Русский\");")
    lines.append("        assert_eq!(Language::English.endonym(), \"English\");")
    lines.append("        for language in Language::ALL {")
    lines.append("            assert!(!language.endonym().is_empty());")
    lines.append("            assert!(!language.as_str().is_empty());")
    lines.append("        }")
    lines.append("    }")
    lines.append("}")
    return "\n".join(lines)


if __name__ == "__main__":
    import io
    import subprocess

    # Checked before anything is written, so a bad table cannot leave half a catalogue
    # on disk for someone to debug.
    keys = [k for k, _, _ in ENTRIES]
    assert len(keys) == len(set(keys)), "duplicate key"
    for key, en, ru in ENTRIES:
        assert en and ru, f"empty translation for {key}"
        assert en.count("{}") == ru.count("{}"), (
            f"{key}: the translations disagree about placeholders"
        )

    io.open('apps/ui/ui/strings.slint', 'w', encoding='utf-8').write(slint_global())
    io.open('apps/ui/src/i18n.rs', 'w', encoding='utf-8').write(rust_catalogue())

    # Formatted here rather than left to the developer. CI runs `cargo fmt --check`, so a
    # regeneration that skipped this would fail the build for a file nobody hand-edited.
    try:
        subprocess.run(
            ['rustfmt', '--edition', '2024', 'apps/ui/src/i18n.rs'],
            check=True,
            capture_output=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"warning: could not run rustfmt ({error}); run `cargo fmt --all`")

    print(f"generated {len(ENTRIES)} strings")
