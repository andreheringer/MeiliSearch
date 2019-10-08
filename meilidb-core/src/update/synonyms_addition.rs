use std::collections::BTreeMap;
use std::sync::Arc;

use fst::{SetBuilder, set::OpBuilder};
use sdset::SetBuf;

use crate::automaton::normalize_str;
use crate::raw_indexer::RawIndexer;
use crate::serde::{extract_document_id, Serializer, RamDocumentStore};
use crate::store;
use crate::update::push_synonyms_addition;
use crate::{MResult, Error, RankedMap};

pub struct SynonymsAddition {
    updates_store: store::Updates,
    updates_results_store: store::UpdatesResults,
    updates_notifier: crossbeam_channel::Sender<()>,
    synonyms: BTreeMap<String, Vec<String>>,
}

impl SynonymsAddition {
    pub fn new(
        updates_store: store::Updates,
        updates_results_store: store::UpdatesResults,
        updates_notifier: crossbeam_channel::Sender<()>,
    ) -> SynonymsAddition
    {
        SynonymsAddition {
            updates_store,
            updates_results_store,
            updates_notifier,
            synonyms: BTreeMap::new(),
        }
    }

    pub fn add_synonym<S, T, I>(&mut self, synonym: S, alternatives: I)
    where S: AsRef<str>,
          T: AsRef<str>,
          I: IntoIterator<Item=T>,
    {
        let synonym = normalize_str(synonym.as_ref());
        let alternatives = alternatives.into_iter().map(|s| s.as_ref().to_lowercase());
        self.synonyms.entry(synonym).or_insert_with(Vec::new).extend(alternatives);
    }

    pub fn finalize(self, mut writer: rkv::Writer) -> MResult<u64> {
        let update_id = push_synonyms_addition(
            &mut writer,
            self.updates_store,
            self.updates_results_store,
            self.synonyms,
        )?;
        writer.commit()?;
        let _ = self.updates_notifier.send(());

        Ok(update_id)
    }
}

pub fn apply_synonyms_addition(
    writer: &mut rkv::Writer,
    main_store: store::Main,
    synonyms_store: store::Synonyms,
    addition: BTreeMap<String, Vec<String>>,
) -> Result<(), Error>
{
    let mut synonyms_builder = SetBuilder::memory();

    for (word, alternatives) in addition {
        synonyms_builder.insert(&word).unwrap();

        let alternatives = {
            let alternatives = SetBuf::from_dirty(alternatives);
            let mut alternatives_builder = SetBuilder::memory();
            alternatives_builder.extend_iter(alternatives).unwrap();
            let bytes = alternatives_builder.into_inner().unwrap();
            fst::Set::from_bytes(bytes).unwrap()
        };

        synonyms_store.put_synonyms(writer, word.as_bytes(), &alternatives)?;
    }

    let delta_synonyms = synonyms_builder
        .into_inner()
        .and_then(fst::Set::from_bytes)
        .unwrap();

    let synonyms = match main_store.synonyms_fst(writer)? {
        Some(synonyms) => {
            let op = OpBuilder::new()
                .add(synonyms.stream())
                .add(delta_synonyms.stream())
                .r#union();

            let mut synonyms_builder = SetBuilder::memory();
            synonyms_builder.extend_stream(op).unwrap();
            synonyms_builder
                .into_inner()
                .and_then(fst::Set::from_bytes)
                .unwrap()
        },
        None => delta_synonyms,
    };

    main_store.put_synonyms_fst(writer, &synonyms)?;

    Ok(())
}
