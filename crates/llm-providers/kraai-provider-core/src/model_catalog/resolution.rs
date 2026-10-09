use super::{CatalogModel, Snapshot};

impl Snapshot {
    pub(super) fn serving_model(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
    ) -> Option<(&str, &str, &CatalogModel)> {
        self.exact_model(self.serving_provider(provider, api).ok()??, model)
    }

    pub(super) fn owned_model(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
        owner: Option<&str>,
    ) -> Option<(&str, &str, &CatalogModel)> {
        if self.serving_provider(provider, api).ok()?.is_some() {
            return None;
        }
        let owner = owner?.trim();
        if self.providers.contains_key(owner) {
            return self.exact_model(owner, model);
        }
        let mut providers = self.providers.iter().filter(|(_, provider)| {
            provider
                .name
                .as_deref()
                .is_some_and(|name| name.eq_ignore_ascii_case(owner))
        });
        let (provider, _) = providers.next()?;
        if providers.next().is_some() {
            return None;
        }
        self.exact_model(provider, model)
    }

    pub(super) fn model(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
    ) -> Option<(&str, &str, &CatalogModel)> {
        if let Some(provider) = self.serving_provider(provider, api).ok()? {
            return self.exact_model(provider, model);
        }
        if let Some(resolved) = self.canonical_model(model) {
            return Some(resolved);
        }
        let mut candidates = self.providers.iter().filter_map(|(provider, catalog)| {
            catalog.models.get_key_value(model).map(|(id, entry)| {
                if let Some(canonical) = &entry.canonical_model_id {
                    self.canonical_model(canonical)
                } else {
                    Some((provider.as_str(), id.as_str(), entry))
                }
            })
        });
        let resolved = candidates.next()??;
        for candidate in candidates {
            let candidate = candidate?;
            if (candidate.0, candidate.1) != (resolved.0, resolved.1) {
                return None;
            }
        }
        Some(resolved)
    }

    fn serving_provider(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
    ) -> Result<Option<&str>, ()> {
        if let Some(provider) = provider {
            return self
                .providers
                .get_key_value(provider)
                .map(|(id, _)| Some(id.as_str()))
                .ok_or(());
        }
        let Some(api) = api else { return Ok(None) };
        let api = api.trim_end_matches('/');
        let mut matches = self.providers.iter().filter(|(_, provider)| {
            provider
                .api
                .as_deref()
                .is_some_and(|url| url.trim_end_matches('/') == api)
        });
        if let Some((provider, _)) = matches.next() {
            return if matches.next().is_none() {
                Ok(Some(provider))
            } else {
                Err(())
            };
        }
        if api == "https://api.groq.com/openai/v1" {
            return self
                .providers
                .get_key_value("groq")
                .map(|(id, _)| Some(id.as_str()))
                .ok_or(());
        }
        Ok(None)
    }

    fn exact_model(&self, provider: &str, model: &str) -> Option<(&str, &str, &CatalogModel)> {
        let (provider, catalog) = self.providers.get_key_value(provider)?;
        let (model, entry) = catalog.models.get_key_value(model)?;
        Some((provider, model, entry))
    }

    pub(super) fn canonical_model(&self, canonical: &str) -> Option<(&str, &str, &CatalogModel)> {
        let (provider, model) = canonical.split_once('/')?;
        let resolved = self.exact_model(provider, model)?;
        if resolved
            .2
            .canonical_model_id
            .as_deref()
            .is_some_and(|identity| identity != canonical)
        {
            return None;
        }
        Some(resolved)
    }
}
