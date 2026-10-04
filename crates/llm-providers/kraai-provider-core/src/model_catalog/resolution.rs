use super::{CatalogModel, Snapshot};

impl Snapshot {
    pub(super) fn model(
        &self,
        provider: Option<&str>,
        api: Option<&str>,
        model: &str,
    ) -> Option<(&str, &str, &CatalogModel)> {
        if let Some(provider) = provider {
            return self.exact_model(provider, model);
        }
        if let Some(api) = api {
            let api = api.trim_end_matches('/');
            let mut matches = self.providers.iter().filter(|(_, provider)| {
                provider
                    .api
                    .as_deref()
                    .is_some_and(|url| url.trim_end_matches('/') == api)
            });
            if let Some((provider, _)) = matches.next() {
                if matches.next().is_some() {
                    return None;
                }
                return self.exact_model(provider, model);
            }
            if api == "https://api.groq.com/openai/v1" {
                return self.exact_model("groq", model);
            }
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
