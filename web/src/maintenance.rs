use crate::{api, state::ControlState};
use dioxus::prelude::*;
use serde_json::{json, Value};

#[component]
pub fn MaintenancePage(state: ControlState) -> Element {
    let mut tab = use_signal(|| "packages".to_owned());
    let duid = (state.selected)();
    rsx! {
        document::Stylesheet {href:asset!("/assets/maintenance.css")}
        div {class:"maintenance-page",
            div {class:"mode-tabs",role:"tablist",aria_label:"Обслуживание",
                for (id,label) in [("packages","Пакеты и службы"),("sounds","Системные звуки"),("backups","Резервные копии"),("app","Приложение")] {
                    button {class:if tab()==id {"mode-tab active"}else{"mode-tab"},role:"tab",aria_selected:tab()==id,onclick:move |_|tab.set(id.into()),"{label}"}
                }
            }
            if tab()=="packages" {crate::packages::PackagesPage {state}}
            else if tab()=="sounds" {crate::system_sounds::SystemSoundsPage {state}}
            else if tab()=="backups" {
                if duid.is_empty() {section {class:"card empty-state",h2 {"Выберите колонку"}}}
                else {Backups {key:"{duid}",state,duid}}
            } else {
                section {class:"card settings-card",
                    h2 {"Quasar как приложение"}
                    p {"Закрепите пульт на домашнем экране телефона или в панели приложений компьютера."}
                    button {class:"button primary","data-install-quasar":"",disabled:true,"Установить приложение"}
                    p {class:"field-help","data-install-quasar-note":"","Установка доступна через HTTPS или на localhost. В Safari используйте «Поделиться → На экран Домой»."}
                    p {class:"field-help","Управление колонкой требует связи с сервером. Данные аккаунта и команды не сохраняются в офлайн-кэше."}
                }
            }
        }
    }
}

#[component]
fn Backups(state: ControlState, duid: String) -> Element {
    let mut snapshots = use_signal(Vec::<Value>::new);
    let mut busy = use_signal(|| false);
    let mut message = use_signal(|| None::<String>);
    let mut persistent = use_signal(|| true);
    let mut pending = use_signal(|| None::<(String, Option<String>)>);
    let mut confirmation = use_signal(String::new);
    let url = format!("/devices/{}/maintenance", api::encoded(&duid));
    let initial_url = url.clone();
    use_future(move || {
        let url = initial_url.clone();
        async move {
            match api::get::<Value>(state, &url).await {
                Ok(value) => {
                    snapshots.set(value["snapshots"].as_array().cloned().unwrap_or_default());
                    persistent.set(value["persistent"] != false);
                }
                Err(error) => message.set(Some(error)),
            }
        }
    });
    let mut perform = move |url: String, action: String, id: Option<String>, confirmed: bool| {
        if busy() {
            return;
        }
        busy.set(true);
        message.set(None);
        pending.set(None);
        confirmation.set(String::new());
        spawn(async move {
            match api::mutate(
                state,
                "POST",
                &url,
                Some(&json!({"action":action,"id":id,"confirmed":confirmed})),
            )
            .await
            {
                Ok(response) => match response.json::<Value>().await {
                    Ok(value) => message.set(value["message"].as_str().map(str::to_owned)),
                    Err(_) => message.set(Some(
                        "Не удалось прочитать результат. Обновите список перед повтором.".into(),
                    )),
                },
                Err(error) => message.set(Some(error)),
            }
            match api::get::<Value>(state, &url).await {
                Ok(value) => {
                    snapshots.set(value["snapshots"].as_array().cloned().unwrap_or_default())
                }
                Err(error) => message.set(Some(error)),
            }
            busy.set(false);
        });
    };
    rsx! {
        section {class:"card settings-card",
            div {class:"card-heading",div {h2 {"Резервные копии компонентов"}p {"Архивы сохраняются на колонке; скачайте важные копии на компьютер."}}
                button {class:"button primary",disabled:busy(),onclick:{let url=url.clone();move |_|perform(url.clone(),"backup".into(),None,false)},if busy(){"Выполняется…"}else{"Создать копию"}}
            }
            p {class:"field-help","В архив входят установленные через пульт звуки, офлайн-будильники, Bluetooth-вывод и собственная установка YAM. Системный раздел, чужие компоненты и настройки сервера не заменяются. Лимит: 32 МиБ на архив, 20 копий."}
            if !persistent(){p {class:"notice error","База данных недоступна: список и контрольные суммы копий не переживут перезапуск сервера."}}
            if let Some(text)=message(){p {class:"notice",role:"status","{text}"}}
            div {class:"backup-list",
                for item in snapshots(){
                    {let id=item["id"].as_str().unwrap_or_default().to_owned();let size=item["bytes"].as_u64().unwrap_or(0) as f64/1048576.0;
                    let date=item["created_at"].as_str().unwrap_or_default().to_owned();let href=format!("{url}/{}",api::encoded(&id));
                    rsx!{article {class:"backup-item",key:"{id}",
                        div {strong {"{date}"}p {class:"field-help","{size:.2} МиБ · SHA-256 проверяется перед восстановлением"}}
                        div {class:"backup-actions",
                            a {class:"button secondary",href,download:"quasar-backup.tar.gz","Скачать"}
                            button {class:"button secondary",disabled:busy(),onclick:{let id=id.clone();move |_|{pending.set(Some(("restore".into(),Some(id.clone()))));confirmation.set(String::new());}},"Восстановить"}
                            button {class:"button secondary",disabled:busy(),onclick:move |_|{pending.set(Some(("delete".into(),Some(id.clone()))));confirmation.set(String::new());},"Удалить копию"}
                        }
                    }}}
                }
                if snapshots().is_empty(){p {class:"field-help","Сохранённых копий пока нет."}}
            }
        }
        section {class:"card settings-card",
            h2 {"Удалить установленные компоненты"}
            p {"Сначала создаётся резервная копия, затем останавливаются службы и удаляются только компоненты, которыми управляет пульт. Сама колонка и сервер продолжают работать."}
            button {class:"button secondary",disabled:busy(),onclick:move |_|{pending.set(Some(("uninstall".into(),None)));confirmation.set(String::new());},"Удалить компоненты пульта"}
        }
        if let Some((action,id))=pending(){
            section {class:"card settings-card backup-confirm",role:"alert",h3 {"Подтверждение"}
                p {if action=="restore" {"Текущие файлы будут заменены после создания копии. Компоненты останутся остановленными; включите их снова в соответствующих разделах."}
                else if action=="uninstall" {"Будут удалены компоненты на колонке, установленные через этот пульт. Архивы сохранятся."}
                else {"Выбранный архив будет удалён с колонки без возможности восстановления."}}
                label {class:"form-field",span {"Введите ДА для подтверждения"}input {value:"{confirmation}",oninput:move |event|confirmation.set(event.value()),autocomplete:"off"}}
                div {class:"backup-actions",
                    button {class:"button primary",disabled:busy()||confirmation()!="ДА",onclick:{let url=url.clone();move |_|perform(url.clone(),action.clone(),id.clone(),true)},"Подтвердить"}
                    button {class:"button secondary",onclick:move |_|pending.set(None),"Отмена"}
                }
            }
        }
    }
}
