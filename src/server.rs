use futures::Stream;
use std::borrow::BorrowMut;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tonic::{Request, Response, Status};

use crate::store::inventory_server::Inventory;
use crate::store::{
    self, InventoryChangeResponse, InventoryUpdateResponse, Item, ItemIdentifier,
    PriceChangeRequest,
};

const BAD_PRICE_ERR: &str = "provided PRICE was invalid";
const DUP_PRICE_ERR: &str = "item is already at this price";
const DUP_ITEM_ERR: &str = "item already exists in inventory";
const DUP_QUANT_ERR: &str = "item is already at this quantity";
const LOW_QUANT_ERR: &str = "invalid decrease quantity cannot bigger than current quantity";
const EMPTY_SKU_ERR: &str = "provided SKU was empty";
const NO_ID_ERR: &str = "no ID or SKU provided for item";
const NO_ITEM_ERR: &str = "the item requested was not found";
const NO_STOCK_ERR: &str = "no stock provided for item";

#[derive(Debug)]
pub struct StoreInventory {
    inventory: Arc<Mutex<HashMap<String, Item>>>,
}

impl Default for StoreInventory {
    fn default() -> Self {
        StoreInventory {
            inventory: Arc::new(Mutex::new(HashMap::<String, Item>::new())),
        }
    }
}

#[tonic::async_trait]
impl Inventory for StoreInventory {
    async fn add(
        &self,
        request: tonic::Request<crate::store::Item>,
    ) -> Result<tonic::Response<crate::store::InventoryChangeResponse>, tonic::Status> {
        let item = request.into_inner();

        let sku = match item.identifier.as_ref() {
            Some(id) if id.sku.is_empty() => return Err(Status::invalid_argument(EMPTY_SKU_ERR)),
            Some(id) => id.sku.to_owned(),
            None => return Err(Status::invalid_argument(NO_ID_ERR)),
        };

        match item.stock.as_ref() {
            Some(stock) if stock.price <= 0.00 => {
                return Err(Status::invalid_argument(BAD_PRICE_ERR))
            }
            Some(_) => {}
            None => return Err(Status::invalid_argument(NO_STOCK_ERR)),
        };

        let mut map = self.inventory.lock().await;
        if map.get(&sku).is_some() {
            return Err(Status::already_exists(DUP_ITEM_ERR));
        }

        map.insert(sku, item);

        Ok(Response::new(InventoryChangeResponse {
            status: "success".into(),
        }))
    }

    async fn remove(
        &self,
        request: tonic::Request<crate::store::ItemIdentifier>,
    ) -> Result<tonic::Response<crate::store::InventoryChangeResponse>, tonic::Status> {
        let item = request.into_inner();

        if item.sku.is_empty() {
            return Err(Status::invalid_argument(EMPTY_SKU_ERR));
        }

        let mut map = self.inventory.lock().await;
        let response = match map.remove(&item.sku) {
            Some(_) => "success: item was removed",
            None => "sucsees: item did not exist",
        };

        Ok(Response::new(InventoryChangeResponse {
            status: response.into(),
        }))
    }

    async fn get(
        &self,
        request: tonic::Request<crate::store::ItemIdentifier>,
    ) -> Result<tonic::Response<crate::store::Item>, tonic::Status> {
        let item = request.into_inner();

        if item.sku.is_empty() {
            return Err(Status::invalid_argument(EMPTY_SKU_ERR));
        }

        let map = self.inventory.lock().await;
        let response = match map.get(&item.sku) {
            Some(response) => response,
            None => return Err(Status::not_found(NO_ITEM_ERR)),
        };

        Ok(Response::new(response.clone()))
    }

    async fn get_all(
        &self,
        _request: tonic::Request<crate::store::ItemAll>,
    ) -> Result<tonic::Response<crate::store::Items>, tonic::Status> {
        let map = self.inventory.lock().await;

        let items = map.values().cloned().collect();
        let response = store::Items { items };

        Ok(Response::new(response))
    }

    async fn decrease_quantity(
        &self,
        request: tonic::Request<store::QuantityChangeRequest>,
    ) -> Result<tonic::Response<store::InventoryUpdateResponse>, tonic::Status> {
        let item = request.into_inner();
        let mut map = self.inventory.lock().await;
        let quantity = match map.get_mut(&item.sku) {
            Some(quantity) => quantity,
            None => return Err(Status::not_found(NO_ITEM_ERR)),
        };

        let stock = match quantity.stock.borrow_mut() {
            Some(stock) => stock,
            None => return Err(Status::internal(NO_STOCK_ERR)),
        };

        if item.sku.is_empty() {
            return Err(Status::invalid_argument(EMPTY_SKU_ERR));
        }

        if item.quantity == 0 {
            return Err(Status::invalid_argument(DUP_QUANT_ERR));
        }

        stock.quantity = match item.quantity {
            item if item > stock.quantity => {
                return Err(Status::invalid_argument(LOW_QUANT_ERR));
            }

            item => stock.quantity - item,
        };

        Ok(Response::new(InventoryUpdateResponse {
            status: "success".into(),
            price: stock.price,
            quantity: stock.quantity,
        }))
    }

    async fn increase_quantity(
        &self,
        request: tonic::Request<store::QuantityChangeRequest>,
    ) -> Result<tonic::Response<store::InventoryUpdateResponse>, tonic::Status> {
        let item = request.into_inner();
        let mut map = self.inventory.lock().await;
        let quantity = match map.get_mut(&item.sku) {
            Some(quantity) => quantity,
            None => return Err(Status::not_found(NO_ITEM_ERR)),
        };

        let stock = match quantity.stock.borrow_mut() {
            Some(stock) => stock,
            None => return Err(Status::internal(NO_STOCK_ERR)),
        };

        if item.sku.is_empty() {
            return Err(Status::invalid_argument(EMPTY_SKU_ERR));
        }

        if item.quantity == 0 {
            return Err(Status::invalid_argument(DUP_QUANT_ERR));
        }

        let item = item.quantity;
        stock.quantity = stock.quantity + item;

        Ok(Response::new(InventoryUpdateResponse {
            status: "success".into(),
            price: stock.price,
            quantity: stock.quantity,
        }))
    }

    async fn update_price(
        &self,
        request: Request<PriceChangeRequest>,
    ) -> Result<Response<InventoryUpdateResponse>, Status> {
        let item = request.into_inner();

        if item.sku.is_empty() {
            return Err(Status::invalid_argument(EMPTY_SKU_ERR));
        }

        if item.price <= 0.0 {
            return Err(Status::invalid_argument(BAD_PRICE_ERR));
        }

        let mut map = self.inventory.lock().await;
        let price = match map.get_mut(&item.sku) {
            Some(price) => price,
            None => return Err(Status::not_found(NO_ITEM_ERR)),
        };

        let stock = match price.stock.borrow_mut() {
            Some(stock) => stock,
            None => return Err(Status::internal(NO_STOCK_ERR)),
        };

        if stock.price == item.price {
            return Err(Status::invalid_argument(DUP_PRICE_ERR));
        }

        stock.price = item.price;

        Ok(Response::new(InventoryUpdateResponse {
            status: "success".into(),
            price: stock.price,
            quantity: stock.quantity,
        }))
    }

    type WatchStream = Pin<Box<dyn Stream<Item = Result<Item, Status>> + Send>>;

    async fn watch(
        &self,
        request: Request<ItemIdentifier>,
    ) -> Result<Response<Self::WatchStream>, Status> {
        let id = request.into_inner();
        let mut item = self.get(Request::new(id.clone())).await?.into_inner();

        let (tx, rx) = mpsc::unbounded_channel();

        let inventory = self.inventory.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;

                let map = inventory.lock().await;
                let item_refresh = match map.get(&id.sku) {
                    Some(item) => item,
                    None => {
                        if let Err(err) = tx.send(Err(Status::not_found(NO_ITEM_ERR))) {
                            println!("ERROR: failed to update stream client: {:?}", err);
                        }
                        return;
                    }
                };

                if item_refresh != &item {
                    if let Err(err) = tx.send(Ok(item_refresh.clone())) {
                        println!("ERROR: failed to update stream client: {:?}", err);
                        return;
                    }
                }

                item = item_refresh.clone()
            }
        });

        let stream = UnboundedReceiverStream::new(rx);
        Ok(Response::new(Box::pin(stream) as Self::WatchStream))
    }
}
